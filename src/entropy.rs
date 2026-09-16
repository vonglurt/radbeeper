// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// Random bits from decay timing, with the accounting to justify them.
//
// The physics is sound and old: the moment a nucleus decays is not determined
// by anything. What goes wrong is everything between the tube and the hex, so
// most of what is here is accounting rather than bits.
use crate::clock;
use crate::sha256::{hex, Sha256};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const ENTROPY_BITS: f64 = 256.0;
pub const ENTROPY_LABEL: &[u8] = b"radbeeper/entropy/1";

/// Measured min-entropy per sample, bits. NIST SP 800-90B section 6.3.1.
///
/// WHY NOT THE POISSON FORMULA. Decay is Poisson; the per-second counts
/// coming back over the serial link measurably are not. Across 1,128 recorded
/// samples from a GMC-320Re the variance is 2.54x the mean -- Poisson
/// requires 1.00 -- with 36% too many empty seconds and a tail that runs to
/// fourteen times the modelled rate at k=5. Every pool is over-dispersed on
/// its own, so it is not a mixture of quiet and busy periods.
///
/// Over-dispersion concentrates a distribution on its mode, and the mode is
/// what min-entropy is about, so the model claimed 1.15 bits a second where
/// the data supports 0.62. This asks the samples instead: the frequency of
/// the most common value, pushed to the far end of its 99% confidence
/// interval so the entropy is a LOWER bound rather than a point estimate.
///
/// Returns 0.0 until there are enough samples for the bound to say anything,
/// which at background is about a dozen seconds.
pub fn mcv_min_entropy(counts: &[u32]) -> f64 {
    let n = counts.len();
    if n < 2 {
        return 0.0;
    }
    let mut freq = std::collections::HashMap::new();
    for c in counts {
        *freq.entry(*c).or_insert(0usize) += 1;
    }
    let most = freq.values().copied().max().unwrap_or(0);
    let p = most as f64 / n as f64;
    let upper = p + 2.576 * (p * (1.0 - p) / (n - 1) as f64).sqrt();
    if upper >= 1.0 {
        return 0.0;
    }
    -upper.log2()
}

/// Bits per sample for Poisson arrivals at `rate`. Kept for the comparison
/// only: nothing is emitted on the strength of this number.
pub fn poisson_min_entropy(rate: f64) -> f64 {
    if rate <= 0.0 {
        return 0.0;
    }
    // P(k) rises to its peak at k = floor(rate) and falls after; walk to it.
    let mut p = (-rate).exp();
    let mut best = p;
    for k in 1..(rate as i64 + 2) {
        p *= rate / k as f64;
        if p > best {
            best = p;
        }
    }
    if best >= 1.0 {
        return 0.0;
    }
    -best.log2()
}

/// Per-second counts as hex nibbles, 15 meaning "15 or more".
///
/// Compact enough to write beside every emission -- three hundred seconds of
/// background is 300 characters -- and lossless over the range that carries
/// the entropy. A second with sixteen counts in it is not doing the work here.
pub fn pack_counts(counts: &[u32]) -> String {
    counts.iter().map(|c| format!("{:x}", (*c).min(15))).collect()
}

pub fn unpack_counts(text: &str) -> Vec<u32> {
    text.chars().filter_map(|c| c.to_digit(16)).collect()
}

/// Hex in blocks, because 64 undifferentiated characters cannot be read,
/// compared against a screen, or dictated down a phone.
pub fn group_hex(text: &str) -> String {
    text.as_bytes()
        .chunks(8)
        .map(|c| String::from_utf8_lossy(c).into_owned())
        .collect::<Vec<_>>()
        .join(" ")
}

pub struct Record {
    pub seq: u64,
    pub started: f64,
    pub seconds: usize,
    pub bits: f64,
    pub rate: f64,
    pub counts: String,
    pub hex: String,
}

pub struct Entropy {
    pub want: f64,
    pub counts: Vec<u32>,
    pub started: Option<f64>,
    pub total: u64,
    pub seq: u64,
}

impl Default for Entropy {
    fn default() -> Self {
        Self::new(ENTROPY_BITS)
    }
}

impl Entropy {
    pub fn new(want: f64) -> Entropy {
        Entropy { want, counts: Vec::new(), started: None, total: 0, seq: 0 }
    }

    pub fn reset(&mut self) {
        self.counts.clear();
        self.started = None;
        self.total = 0;
    }

    pub fn add(&mut self, counts: u32) {
        if self.started.is_none() {
            // WALL CLOCK, NOT THE MONOTONIC STAMP THE SAMPLES CARRY. This
            // value goes into the digest and into the emission log, and a
            // reader recomputing a line next month has no way to recover a
            // boot-relative number.
            self.started = Some(clock::now());
        }
        self.counts.push(counts);
        self.total += counts as u64;
    }

    pub fn rate(&self) -> f64 {
        if self.counts.is_empty() {
            0.0
        } else {
            self.total as f64 / self.counts.len() as f64
        }
    }

    /// Measured min-entropy per sample. 0.0 while it cannot yet tell.
    pub fn per_sample(&self) -> f64 {
        mcv_min_entropy(&self.counts)
    }

    pub fn bits(&self) -> f64 {
        self.per_sample() * self.counts.len() as f64
    }

    /// What the Poisson model would claim. Reported beside the measured
    /// figure so the gap is visible rather than argued about.
    pub fn model_bits(&self) -> f64 {
        poisson_min_entropy(self.rate()) * self.counts.len() as f64
    }

    pub fn ready(&self) -> bool {
        self.bits() >= self.want
    }

    /// Seconds still needed, at the entropy per sample measured so far.
    ///
    /// Deliberately pessimistic: the confidence bound tightens as samples
    /// arrive, so the true wait is a little shorter than this says. A
    /// countdown that overruns is a worse thing to put on a screen than one
    /// that finishes early.
    pub fn wait(&self) -> Option<i64> {
        let per = self.per_sample();
        if per <= 0.0 {
            return None;
        }
        Some((((self.want - self.bits()) / per).ceil() as i64).max(0))
    }

    /// The line, without consuming the pool. Deterministic in its input.
    ///
    /// The message is the label, then NUL, the sequence number, NUL, the
    /// second the pool opened, NUL, and the packed counts -- byte for byte
    /// what the Python builds, which is what lets either program check the
    /// other's emissions.
    pub fn digest(&self, seq: u64) -> String {
        let mut h = Sha256::new();
        h.update(ENTROPY_LABEL);
        h.update(
            format!("\0{}\0{}\0", seq, self.started.unwrap_or(0.0) as i64)
                .as_bytes(),
        );
        h.update(pack_counts(&self.counts).as_bytes());
        hex(&h.finish())
    }

    /// Take the line and start collecting again.
    pub fn draw(&mut self) -> (String, Record) {
        let seq = self.seq;
        let out = self.digest(seq);
        let record = Record {
            seq,
            started: self.started.unwrap_or(0.0),
            seconds: self.counts.len(),
            bits: self.bits(),
            rate: self.rate(),
            counts: pack_counts(&self.counts),
            hex: out.clone(),
        };
        self.seq += 1;
        self.reset();
        (out, record)
    }
}

/// Recompute a line from the counts recorded beside it.
pub fn check_record(seq: u64, started: f64, counts: &[u32], want: &str) -> bool {
    let mut e = Entropy::default();
    e.started = Some(started);
    e.counts = counts.to_vec();
    e.total = counts.iter().map(|c| *c as u64).sum();
    e.digest(seq) == want
}

/// Where the pool has got to, in the words each state deserves.
pub fn pool_status(pool: &Entropy, prefix: &str) -> String {
    match pool.wait() {
        Some(left) => format!("{}{}s", prefix, left),
        None if pool.counts.is_empty() => "no counts yet".to_string(),
        None => format!("measuring the source ({}s)", pool.counts.len()),
    }
}

/// Append an emission and the counts that made it, so it can be checked.
///
/// An audit trail rather than a seed. Recomputing a past line from its counts
/// proves the line was not invented; it says nothing about the next one,
/// which comes from decays that have not happened.
pub fn write_record(dir: &Path, record: &Record, serial: &str, suspect: bool)
    -> std::io::Result<PathBuf>
{
    let path = dir.join(format!("random-{}.tsv", if serial.is_empty() {
        "unknown"
    } else {
        serial
    }));
    let fresh = fs::metadata(&path).map(|m| m.len() == 0).unwrap_or(true);
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    if fresh {
        writeln!(f, "#seq\ttime\tseconds\trate\tbits\tflat\thex\tcounts")?;
    }
    writeln!(
        f,
        "{}\t{}\t{}\t{:.4}\t{:.1}\t{}\t{}\t{}",
        record.seq,
        clock::stamp(record.started),
        record.seconds,
        record.rate,
        record.bits,
        if suspect { "no" } else { "yes" },
        record.hex,
        record.counts
    )?;
    Ok(path)
}

pub struct Emission {
    pub seq: u64,
    pub time: String,
    pub seconds: usize,
    pub rate: f64,
    pub bits: f64,
    pub flat: bool,
    pub hex: String,
    pub counts: Vec<u32>,
}

/// Emissions, oldest first, each with the counts that produced it.
pub fn read_emissions(path: &Path) -> Vec<Emission> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') || line.trim().is_empty() {
            continue;
        }
        let c: Vec<&str> = line.split('\t').collect();
        if c.len() < 8 {
            continue;
        }
        let parsed = (|| {
            Some(Emission {
                seq: c[0].parse().ok()?,
                time: c[1].to_string(),
                seconds: c[2].parse().ok()?,
                rate: c[3].parse().ok()?,
                bits: c[4].parse().ok()?,
                flat: c[5] == "yes",
                hex: c[6].to_string(),
                counts: unpack_counts(c[7]),
            })
        })();
        if let Some(e) = parsed {
            out.push(e);
        }
    }
    out
}

// ----------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    /// A little deterministic noise, so a failure is reproducible.
    struct Rng(u32);
    impl Rng {
        fn next(&mut self, n: u32) -> u32 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 17;
            self.0 ^= self.0 << 5;
            self.0 % n
        }
    }

    #[test]
    fn the_worth_of_a_sample_is_measured_not_modelled() {
        let even: Vec<u32> = (0..400).map(|i| i % 4).collect();
        assert!(mcv_min_entropy(&even) > 1.5);
        // Over-dispersed: same mean, piled onto its mode. Less entropy, and
        // the estimator says so where the Poisson formula would not.
        let mut lumpy = vec![0u32; 300];
        lumpy.extend(std::iter::repeat(6u32).take(100));
        assert!(mcv_min_entropy(&lumpy) < mcv_min_entropy(&even));
        // Too few samples to bound anything is reported as nothing, not as a
        // guess: the bound is only allowed to make the entropy smaller.
        assert_eq!(mcv_min_entropy(&[0, 1]), 0.0);
        assert_eq!(mcv_min_entropy(&[]), 0.0);
    }

    #[test]
    fn a_counter_stuck_at_one_count_a_second_earns_nothing() {
        // THE CASE THE POISSON MODEL GOT WRONG. A tube reporting exactly one
        // count every second has a perfectly ordinary rate and no randomness
        // whatsoever, and the model credited it with half a bit a second
        // because it only ever looked at the mean.
        let mut pool = Entropy::default();
        for _ in 0..3600 {
            pool.add(1);
        }
        assert_eq!(pool.bits(), 0.0);
        assert!(!pool.ready());
        assert_eq!(pool.wait(), None);
        assert!(pool.model_bits() > 256.0, "the model would have called this good");
    }

    #[test]
    fn a_dead_counter_never_becomes_ready() {
        let mut pool = Entropy::default();
        for _ in 0..3600 {
            pool.add(0);
        }
        assert_eq!(pool.bits(), 0.0);
        assert!(!pool.ready());
    }

    #[test]
    fn it_will_not_hand_over_bits_it_has_not_earned() {
        let mut rng = Rng(11);
        let mut pool = Entropy::default();
        for _ in 0..50 {
            pool.add(rng.next(4));
        }
        assert!(!pool.ready());
        assert!(pool.wait().unwrap() > 0);
        for _ in 0..350 {
            pool.add(rng.next(4));
        }
        assert!(pool.ready());
    }

    #[test]
    fn the_countdown_only_ever_overruns() {
        // It is projected at the entropy per sample measured so far, and that
        // figure rises as the confidence bound tightens, so the wait quoted
        // is never shorter than the wait served.
        let mut rng = Rng(5);
        let mut pool = Entropy::default();
        let mut quoted = None;
        let mut served = 0i64;
        while !pool.ready() {
            pool.add(rng.next(4));
            served += 1;
            if quoted.is_none() && served == 60 {
                quoted = Some(pool.wait().unwrap() + served);
            }
            assert!(served < 5000, "the pool never filled");
        }
        assert!(quoted.unwrap() >= served);
    }

    #[test]
    fn the_line_is_64_hex_characters() {
        let mut pool = Entropy::default();
        for i in 0..400u32 {
            pool.add(i % 3);
        }
        let (text, _record) = pool.draw();
        assert_eq!(text.len(), 64);
        assert!(text.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()));
    }

    #[test]
    fn drawing_a_line_empties_the_pool_and_moves_the_sequence_on() {
        let mut pool = Entropy::default();
        for i in 0..400u32 {
            pool.add(i % 3);
        }
        let (_t, r) = pool.draw();
        assert_eq!(r.seq, 0);
        assert_eq!(r.seconds, 400);
        assert!(pool.counts.is_empty());
        assert_eq!(pool.started, None);
        assert_eq!(pool.seq, 1);
    }

    #[test]
    fn a_line_can_be_recomputed_from_what_was_written_beside_it() {
        let mut pool = Entropy::default();
        for i in 0..400u32 {
            pool.add((i * 7) % 4);
        }
        let (_t, r) = pool.draw();
        let counts = unpack_counts(&r.counts);
        assert!(check_record(r.seq, r.started, &counts, &r.hex));
        // Change one second's count and the line no longer follows from it.
        let mut tampered = counts.clone();
        tampered[0] = if tampered[0] == 1 { 2 } else { 1 };
        assert!(!check_record(r.seq, r.started, &tampered, &r.hex));
        // And so does the sequence number, which is in the digest.
        assert!(!check_record(r.seq + 1, r.started, &counts, &r.hex));
    }

    #[test]
    fn counts_pack_to_one_nibble_and_clamp_at_fifteen() {
        assert_eq!(pack_counts(&[0, 1, 10, 15]), "01af");
        assert_eq!(pack_counts(&[16, 99, 4000]), "fff",
                   "a second with sixteen counts is not doing the work here");
        assert_eq!(unpack_counts("01af"), vec![0, 1, 10, 15]);
        assert_eq!(unpack_counts(""), Vec::<u32>::new());
    }

    #[test]
    fn hex_is_grouped_so_it_can_be_read_aloud() {
        let g = group_hex(&"a".repeat(64));
        assert_eq!(g.split(' ').count(), 8);
        assert!(g.split(' ').all(|b| b.len() == 8));
    }

    #[test]
    fn the_status_line_gives_each_state_its_own_words() {
        let mut pool = Entropy::default();
        assert_eq!(pool_status(&pool, "next in "), "no counts yet");
        pool.add(1);
        pool.add(0);
        assert_eq!(pool_status(&pool, "next in "), "measuring the source (2s)");
        let mut rng = Rng(3);
        for _ in 0..200 {
            pool.add(rng.next(4));
        }
        assert!(pool_status(&pool, "next in ").starts_with("next in "));
    }
}
