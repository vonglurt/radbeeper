// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
// Made-up logs in the real format: a month of counts and of temperatures,
// for the tests, and for looking at a paper before the room has filled one.
//
// WHY THIS IS IN THE LIBRARY. A day line needs a day and a month line a
// month, and nobody tests a drawing by waiting thirty days for it. These
// write the files the service writes -- the same header, the same row, the
// same names -- with counts drawn the way decay draws them, and with
// whatever is asked for laid over that: something that comes round on a
// schedule, a break in the record. What reads the real logs reads these,
// and a file made here says `sample` in its site column so that nobody
// takes it for a measurement.
use crate::{clock, log};
use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};

/// The windows a sample log's header names: the program's own defaults.
pub const SPANS: [f64; 5] = [3.0, 30.0, 300.0, 3000.0, 30000.0];

/// A small generator, so a sample is the same sample every time it is made.
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// The next number from nought up to one.
    pub fn unit(&mut self) -> f64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 11) as f64 / (1u64 << 53) as f64
    }

    /// A count the way decay gives one: Poisson, with this mean.
    pub fn poisson(&mut self, mean: f64) -> u64 {
        // Knuth's, in pieces of thirty so the exponential never underflows.
        let mut left = mean.max(0.0);
        let mut k = 0u64;
        while left > 0.0 {
            let part = left.min(30.0);
            left -= part;
            let floor = (-part).exp();
            let mut p = self.unit();
            while p > floor {
                k += 1;
                p *= self.unit();
            }
        }
        k
    }
}

fn write_all(files: BTreeMap<PathBuf, Vec<String>>, head: &str) -> std::io::Result<()> {
    for (path, lines) in files {
        let mut f = std::fs::File::create(&path)?;
        writeln!(f, "{}", head)?;
        for line in lines {
            writeln!(f, "{}", line)?;
        }
    }
    Ok(())
}

/// A counter's count log from `begin` to `end`: a row every thirty seconds
/// at `cps` counts a second, in the files the service would have written.
///
/// `cycles` are (period in seconds, depth): the rate rises and falls by that
/// share of itself with that period, which is what something arriving on a
/// schedule does to it. `gaps` are (from, to): no rows are written in them.
/// A month's file that is already there is replaced.
#[allow(clippy::too_many_arguments)]
pub fn counts(
    directory: &Path,
    serial: &str,
    begin: f64,
    end: f64,
    cps: f64,
    cycles: &[(f64, f64)],
    gaps: &[(f64, f64)],
    seed: u64,
) {
    let every = log::DEFAULT_LOG_EVERY;
    let mut rng = Rng::new(seed);
    let none: Vec<Option<f64>> = vec![None; SPANS.len()];
    let mut files: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut when = (begin / every).ceil() * every + every;
    while when <= end {
        let mid = when - every / 2.0;
        // The draw is made whether or not the row is kept, so a sample with
        // a break in it is the same sample as the one without, elsewhere.
        let rate = cycles.iter().fold(cps, |r, (period, depth)| {
            r + cps * depth * (2.0 * std::f64::consts::PI * mid / period).sin()
        });
        let n = rng.poisson(rate.max(0.0) * every);
        if !gaps.iter().any(|(a, b)| mid >= *a && mid < *b) {
            let row = log::row(when, n as f64 / every, n, every, &none, &none,
                               log::SRC_LIVE, "sample");
            files.entry(log::path(when, directory, Some(serial))).or_default().push(row);
        }
        when += every;
    }
    let _ = write_all(files, &log::header(&SPANS));
}

/// A counter's temperature log from `begin` to `end`, a row every thirty
/// seconds: `base` degrees Celsius, rising and falling `swing` over each
/// day with its warmest in the middle of the afternoon, a slow wander from
/// day to day, and each of `odd` -- (from, to, degrees) -- added on top.
/// To the tenth, as the counter gives it.
#[allow(clippy::too_many_arguments)]
pub fn temps(
    directory: &Path,
    serial: &str,
    begin: f64,
    end: f64,
    base: f64,
    swing: f64,
    odd: &[(f64, f64, f64)],
    seed: u64,
) {
    let every = log::DEFAULT_LOG_EVERY;
    let mut rng = Rng::new(seed);
    let mut files: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut when = (begin / every).ceil() * every;
    let mut wander = 0.0f64;
    while when <= end {
        let hour = (when - clock::day_start(when)) / 3600.0;
        wander = (wander + (rng.unit() - 0.5) * 0.004).clamp(-0.6, 0.6);
        let mut c = base
            + swing / 2.0 * (std::f64::consts::PI * (hour - 9.0) / 12.0).sin()
            + wander
            + (rng.unit() - 0.5) * 0.06;
        for (a, b, more) in odd {
            if when >= *a && when < *b {
                c += more;
            }
        }
        files
            .entry(log::temp_path(when, directory, serial))
            .or_default()
            .push(format!("{}\t{:.1}", clock::stamp(when), c));
        when += every;
    }
    let _ = write_all(files, "#time\ttemp_c");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sample_is_the_same_sample_twice_and_reads_as_a_real_log() {
        let dir = std::env::temp_dir().join(format!("radbeeper-sample-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let end = clock::day_start(1_790_000_000.0) + 7200.0;
        counts(&dir, "AAAAAAAAAAAAAA", end - 3600.0, end, 0.7, &[], &[], 4);
        let path = log::path(end, &dir, Some("AAAAAAAAAAAAAA"));
        let first = std::fs::read_to_string(&path).unwrap();
        counts(&dir, "AAAAAAAAAAAAAA", end - 3600.0, end, 0.7, &[], &[], 4);
        assert_eq!(first, std::fs::read_to_string(&path).unwrap());
        let names = log::columns(&log::header(&SPANS));
        let rows = log::read_table(&path, &names);
        assert_eq!(rows.len(), 120, "an hour of rows, thirty seconds apart");
        let total: f64 = rows.iter().map(|r| r.1[2].parse::<f64>().unwrap()).sum();
        assert!((total / (120.0 * 30.0) - 0.7).abs() < 0.1, "{}", total);
        assert_eq!(rows[0].1.last().map(String::as_str), Some("sample"));

        temps(&dir, "AAAAAAAAAAAAAA", end - 86400.0, end, 22.0, 3.0, &[], 4);
        let t = log::read_temps(&dir, "AAAAAAAAAAAAAA", 0.0);
        assert!(t.len() > 2800);
        let (low, high) = t.iter().fold((f64::MAX, f64::MIN), |(l, h), x| (l.min(x.1), h.max(x.1)));
        assert!(high - low > 2.5 && high - low < 4.5, "a three-degree day: {} to {}", low, high);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_poisson_draw_has_the_mean_it_was_asked_for() {
        let mut rng = Rng::new(7);
        for mean in [0.7, 21.0, 300.0] {
            let n = 4000;
            let total: u64 = (0..n).map(|_| rng.poisson(mean)).sum();
            let got = total as f64 / n as f64;
            assert!((got - mean).abs() < mean.sqrt() * 0.1 + 0.05, "{} for {}", got, mean);
        }
    }
}
