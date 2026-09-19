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
            let cutoff = when - self.spans[i];
            let mut head = self.heads[i];
            while head - self.base < self.samples.len() {
                let (t, c) = self.samples[head - self.base];
                if t > cutoff {
                    break;
                }
                self.sums[i] -= c as u64;
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
        let taper = (0..window)
            .map(|i| {
                0.5 - 0.5
                    * (2.0 * std::f64::consts::PI * i as f64 / (window - 1) as f64).cos()
            })
            .collect();
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
        let n = self.independent();
        if n < 1.0 || self.bins < 2 {
            return f64::INFINITY;
        }
        let x = ((self.bins - 1) as f64).ln() / n;
        1.0 + (2.0 * x).sqrt() + 2.0 * x / 3.0
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

/// How many stretches the strip is cut into. Four: a second a bar at the
/// right, then `k`, `k*k` and `k*k*k` as it ages leftwards.
pub const TIERS: usize = 4;

/// The counts, as a strip that compresses as it ages.
///
/// Four tiers of equal width, newest at the right edge. The rightmost is a
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
/// So: `TIERS` aggregating tiers at 1, 2, 4, 8 seconds, and one interleave
/// tier under them at 1/n. Five in all, whether n is two or nine, which is
/// what the two-counter strip already did. The ratio is 2 between every
/// aggregating tier and n at the single boundary below them -- and that
/// boundary is worth marking, because it is exactly where the strip stops
/// measuring time and starts measuring arrival.
pub fn tiers_interleaved(
    samples: &[f64],
    first: usize,
    width: usize,
    tubes: usize,
) -> Vec<Tier> {
    let tubes = tubes.max(1);
    let count = TIERS + 1;
    if width == 0 || count == 0 {
        return Vec::new();
    }
    let q = width / count;
    let fine_cols = width - q * (count - 1);
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
    for _ in 0..TIERS {
        let c = q as i64;
        let top = (b - 1).div_euclid(step);
        let values: Vec<Option<f64>> = (0..c)
            .map(|j| {
                let g = top - c + 1 + j;
                mean(g * step, (g * step + step).min(b))
            })
            .collect();
        b = (top - c + 1) * step;
        out.push(Tier {
            columns: q,
            seconds: step as f64 / tubes as f64,
            values,
        });
        step *= 2;
    }
    out.reverse();
    out
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
        // Four quarters, the odd three columns to the fine tier, and k = 2:
        // 39*8 + 39*4 + 39*2 + 42 = 588 seconds in 159 columns.
        assert_eq!(t.iter().map(|x| x.columns).collect::<Vec<_>>(), vec![39, 39, 39, 42]);
        assert_eq!(t.iter().map(|x| x.seconds).collect::<Vec<_>>(), vec![8.0, 4.0, 2.0, 1.0]);
        let reach: f64 = t.iter().map(|x| x.columns as f64 * x.seconds).sum();
        assert!(reach >= 512.0, "{}", reach);
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
        assert_eq!(t.len(), TIERS + 1, "five tiers, not eight");
        let seconds: Vec<String> = t.iter().map(|x| bar_seconds(x.seconds)).collect();
        assert_eq!(seconds, vec!["8", "4", "2", "1", "1/9"]);
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
        assert_eq!(seconds, vec!["8", "4", "2", "1", "1/2"]);
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
        let t = tiers(&[], 0, 160, 512);
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
        let t = tiers(&s, 0, 20, 30);
        let (oldest, far, near, fine) = (&t[0], &t[1], &t[2], &t[3]);
        assert_eq!(
            (oldest.columns, far.columns, near.columns, fine.columns),
            (5, 5, 5, 5)
        );
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
    }

    #[test]
    fn a_bar_does_not_change_as_the_strip_scrolls_under_it() {
        let s: Vec<u32> = (0..400).map(|i| (i * 7919 % 13) as u32).collect();
        let before = tiers(&s[..300], 0, 40, 80);
        let after = tiers(&s[..302], 0, 40, 80);
        // Two seconds later, with k = 2, every complete near bar has moved one
        // column left and is otherwise the same bar.
        let k = before[2].seconds as usize;
        assert_eq!(k, 2);
        let b = &before[2].values;
        let a = &after[2].values;
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
}
