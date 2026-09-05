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

pub fn level(cpm: f64) -> Level {
    if cpm >= LEVEL_HIGH {
        Level::High
    } else if cpm >= LEVEL_RAISED {
        Level::Raised
    } else {
        Level::Calm
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
    pub fn chance_max(&self) -> f64 {
        let n = self.independent();
        if n < 1.0 || self.bins < 2 {
            return f64::INFINITY;
        }
        1.0 + ((self.bins - 1) as f64).ln() / n
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
}
