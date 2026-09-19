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
    /// When each of those counts landed, for the frame written beside the
    /// emission.
    ///
    /// NOT IN THE DIGEST, AND DELIBERATELY NOT. The key is derived from the
    /// counts and the second the pool opened, byte for byte what the Python
    /// builds -- putting arrival times into it would make every emission
    /// uncheckable by the other implementation. These are kept alongside so
    /// the raw frame can say *when* each second was, which the counts alone
    /// cannot: a pool that spanned a gap looks exactly like one that did not.
    pub times: Vec<f64>,
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
        Entropy {
            want,
            counts: Vec::new(),
            times: Vec::new(),
            started: None,
            total: 0,
            seq: 0,
        }
    }

    pub fn reset(&mut self) {
        self.counts.clear();
        self.times.clear();
        self.started = None;
        self.total = 0;
    }

    pub fn add(&mut self, counts: u32) {
        let now = clock::now();
        self.add_at(now, counts);
    }

    /// One sample, with the wall clock it arrived on.
    ///
    /// The callers that hold the port already know when a sample landed, and
    /// that is a better answer than asking the clock again a few microseconds
    /// later -- it is the number the log rows and the cascade are cut from,
    /// so the frame agrees with them rather than nearly agreeing.
    pub fn add_at(&mut self, when: f64, counts: u32) {
        if self.started.is_none() {
            // WALL CLOCK, NOT THE MONOTONIC STAMP THE SAMPLES CARRY. This
            // value goes into the digest and into the emission log, and a
            // reader recomputing a line next month has no way to recover a
            // boot-relative number.
            self.started = Some(when);
        }
        self.counts.push(counts);
        self.times.push(when);
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

    /// The raw material behind the line this pool is about to draw.
    ///
    /// Taken BEFORE `draw`, because drawing resets the pool. The gaps are
    /// rounded to whole seconds: the samples are one a second by
    /// construction, and a frame that recorded the microseconds of a cadence
    /// the device does not control would be storing jitter as though it were
    /// signal.
    pub fn frame(&self, seq: u64, suspect: bool) -> Frame {
        let mut samples = Vec::with_capacity(self.counts.len());
        let mut prev: Option<f64> = None;
        for (i, count) in self.counts.iter().enumerate() {
            let when = self.times.get(i).copied();
            let gap = match (prev, when) {
                // The first sample opens the frame, so there is no gap
                // before it: `started` is where it is.
                (None, _) => 0,
                (Some(p), Some(w)) => ((w - p).round().max(0.0)) as u32,
                // No time recorded: the cadence is one a second, and saying
                // so is better than claiming the samples were simultaneous.
                (Some(_), None) => 1,
            };
            if when.is_some() {
                prev = when;
            } else if let Some(p) = prev {
                prev = Some(p + 1.0);
            }
            samples.push((gap, *count));
        }
        Frame {
            seq,
            started: self.started.unwrap_or(0.0) as i64,
            suspect,
            samples,
            key: self.digest(seq),
        }
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
    write_hex(dir, &record.hex, serial)?;
    Ok(path)
}

/// Append the digits alone to `random-<serial>.hex`: the time it was drawn,
/// two spaces, sixty-four hex digits, one line per emission.
///
/// The .tsv is the audit trail and carries the counts; this is the stream,
/// for anything that just wants the numbers -- `tail -f` it, or cut the
/// second field and feed it on.
pub fn write_hex(dir: &Path, hex: &str, serial: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("random-{}.hex", if serial.is_empty() {
        "unknown"
    } else {
        serial
    }));
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    writeln!(f, "{}  {}", clock::stamp(clock::now()), hex)?;
    Ok(path)
}

// ------------------------------------------------------------------ frames ---
//
// THE RAW MATERIAL, KEPT. The `.tsv` beside every emission carries the counts
// as one hex digit each, which is lossless up to fifteen counts in a second
// and clamps above it -- fine for recomputing the key, because the key is
// derived from those same clamped digits, and not fine as a record of what the
// tube actually did. A frame is the unclamped version, and it is what somebody
// auditing the source rather than the arithmetic actually needs.
//
// WHY THERE ARE NO PULSE TIMES IN IT, WHICH IS THE FIRST THING ANYBODY ASKS.
// A frame would ideally be the interval between one decay and the next: that
// is where the entropy physically is, and quantised to a millisecond it would
// carry about ten bits per arrival instead of the 0.62 bits a second this
// counter yields. The GQ protocol cannot say it. `<HEARTBEAT1>>` answers with
// a COUNT once a second and `<GETCPS>>` answers with a count when asked; there
// is no message in the device's vocabulary that reports WHEN a pulse landed.
// One second holding one integer is the whole of the raw material, and a file
// format that implied otherwise would be inventing precision the wire never
// carried. See docs/the-random.md.
//
// SO THE THING WORTH COMPRESSING IS THE CADENCE. A naive record is a timestamp
// and a count per second: twelve bytes, of which eleven say "and then it was a
// second later". The cadence is the default and only departures from it are
// worth a byte, so an ordinary second -- one second after the last one, with a
// count that fits in a byte -- is stored as ONE byte that is the count itself.
// Three hundred seconds of background is a little over three hundred bytes.
//
// AND THAT IS WHY THERE IS NO gzip HERE. Beyond the one-dependency rule this
// crate is built on, a DEFLATE header alone is most of what the encoding
// costs, and a stream of bytes valued 0..5 is exactly the shape a generic
// compressor does worst on. The structure is the compression.

/// Magic and version, at the head of every frame rather than of the file.
///
/// PER FRAME, so the file can be appended to forever, concatenated with
/// another, truncated by a full disk or cut in half by a crash, and still be
/// read: a reader that loses its place scans forward for the next magic and
/// carries on. A header at the top of the file would make the first bad byte
/// the last readable one.
pub const FRAME_MAGIC: &[u8; 4] = b"RBF1";

/// The tag that says "this sample is not the ordinary case".
const FRAME_ESCAPE: u8 = 0xFE;
/// The largest count an ordinary one-byte sample can carry.
const FRAME_INLINE_MAX: u32 = 0xFD;

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn get_varint(buf: &[u8], at: &mut usize) -> Option<u64> {
    let mut v = 0u64;
    let mut shift = 0;
    loop {
        let b = *buf.get(*at)?;
        *at += 1;
        v |= ((b & 0x7f) as u64) << shift;
        if b & 0x80 == 0 {
            return Some(v);
        }
        shift += 7;
        if shift > 63 {
            return None;
        }
    }
}

/// One emission's raw material: every second that went into one key.
#[derive(Debug, Clone, PartialEq)]
pub struct Frame {
    pub seq: u64,
    /// Whole seconds since the epoch, of the first sample -- the same number
    /// that went into the digest, so a frame can recompute its own key.
    pub started: i64,
    /// The spectrum was not flat when this was drawn.
    pub suspect: bool,
    /// `(seconds since the previous sample, counts in that second)`, the
    /// first sample's gap being 0.
    pub samples: Vec<(u32, u32)>,
    /// The key this frame produced, as hex.
    pub key: String,
}

impl Frame {
    /// The counts alone, which is what the digest and the estimators want.
    pub fn counts(&self) -> Vec<u32> {
        self.samples.iter().map(|(_, c)| *c).collect()
    }

    /// How long the frame covers, in seconds.
    pub fn seconds(&self) -> u64 {
        self.samples.iter().map(|(g, _)| *g as u64).sum()
    }

    /// Whether this frame really does produce the key written in it.
    ///
    /// THE POINT OF KEEPING THE RAW DATA AT ALL. An emission is a claim that
    /// sixty-four characters came out of a particular stretch of decay; this
    /// is the stretch, and this recomputes the claim from it. It does not say
    /// anything about the NEXT key, which comes from decays that have not
    /// happened yet.
    pub fn verifies(&self) -> bool {
        check_record(self.seq, self.started as f64, &self.counts(), &self.key)
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(32 + self.samples.len() + 48);
        out.extend_from_slice(FRAME_MAGIC);
        out.push(if self.suspect { 1 } else { 0 });
        put_varint(&mut out, self.seq);
        put_varint(&mut out, self.started.max(0) as u64);
        put_varint(&mut out, self.samples.len() as u64);
        for (gap, count) in &self.samples {
            if *gap == 1 && *count <= FRAME_INLINE_MAX {
                // THE ORDINARY SECOND, IN ONE BYTE. One second after the last
                // one, and a count that fits: the byte is the count.
                out.push(*count as u8);
            } else {
                out.push(FRAME_ESCAPE);
                put_varint(&mut out, *gap as u64);
                put_varint(&mut out, *count as u64);
            }
        }
        let key = hex_bytes(&self.key);
        put_varint(&mut out, key.len() as u64);
        out.extend_from_slice(&key);
        out
    }

    /// One frame from the head of `buf`, and how many bytes it used.
    pub fn decode(buf: &[u8]) -> Option<(Frame, usize)> {
        if buf.len() < 5 || &buf[..4] != FRAME_MAGIC {
            return None;
        }
        let mut at = 4;
        let suspect = buf[at] != 0;
        at += 1;
        let seq = get_varint(buf, &mut at)?;
        let started = get_varint(buf, &mut at)? as i64;
        let n = get_varint(buf, &mut at)? as usize;
        // A length that could not fit in what is left is a damaged frame, not
        // a reason to allocate a gigabyte.
        if n > buf.len() {
            return None;
        }
        let mut samples = Vec::with_capacity(n);
        for _ in 0..n {
            let tag = *buf.get(at)?;
            at += 1;
            if tag == FRAME_ESCAPE {
                let gap = get_varint(buf, &mut at)? as u32;
                let count = get_varint(buf, &mut at)? as u32;
                samples.push((gap, count));
            } else if tag == 0xFF {
                return None;
            } else {
                samples.push((1, tag as u32));
            }
        }
        let klen = get_varint(buf, &mut at)? as usize;
        if klen > buf.len().saturating_sub(at) {
            return None;
        }
        let key = hex(&buf[at..at + klen]);
        at += klen;
        Some((Frame { seq, started, suspect, samples, key }, at))
    }
}

/// Hex back to the bytes it stands for; an odd or invalid digit ends it.
fn hex_bytes(text: &str) -> Vec<u8> {
    let d: Vec<u8> = text
        .bytes()
        .filter_map(|c| (c as char).to_digit(16).map(|v| v as u8))
        .collect();
    d.chunks(2).filter(|p| p.len() == 2).map(|p| (p[0] << 4) | p[1]).collect()
}

/// Append a frame to `random-<serial>.bin`.
pub fn write_frame(dir: &Path, frame: &Frame, serial: &str) -> std::io::Result<PathBuf> {
    let path = dir.join(format!("random-{}.bin", if serial.is_empty() {
        "unknown"
    } else {
        serial
    }));
    let mut f = fs::OpenOptions::new().create(true).append(true).open(&path)?;
    f.write_all(&frame.encode())?;
    f.flush()?;
    Ok(path)
}

/// Every frame in a file, in the order they were written.
///
/// A DAMAGED FRAME COSTS ONE FRAME. Anything that does not decode is skipped
/// by scanning forward to the next magic, so a file cut short by a full disk
/// still yields every complete frame before the cut -- which is the whole
/// reason the magic is per frame.
pub fn read_frames(path: &Path) -> Vec<Frame> {
    let Ok(buf) = fs::read(path) else { return Vec::new() };
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 4 <= buf.len() {
        match Frame::decode(&buf[at..]) {
            // A DECODE IS NOT ENOUGH; IT HAS TO LAND SOMEWHERE. Corrupting a
            // length field inside a frame does not make it fail to parse --
            // it makes it parse as a DIFFERENT frame, one that ends in the
            // middle of the next one and swallows it. So a frame is only
            // accepted when the byte after it is the start of another frame
            // or the end of the file, which is the one check that catches a
            // plausible-looking wrong answer.
            Some((f, used)) if used > 0 && ends_cleanly(&buf, at + used) => {
                out.push(f);
                at += used;
            }
            _ => {
                at += 1;
                while at + 4 <= buf.len() && &buf[at..at + 4] != FRAME_MAGIC {
                    at += 1;
                }
            }
        }
    }
    out
}

/// Whether `at` is the end of the file or the head of the next frame.
fn ends_cleanly(buf: &[u8], at: usize) -> bool {
    at == buf.len() || (at + 4 <= buf.len() && &buf[at..at + 4] == FRAME_MAGIC)
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
                // A counts field with anything but hex in it is a row the
                // Python refuses (int(ch, 16) raises), not one to read around.
                counts: if c[7].chars().all(|ch| ch.is_ascii_hexdigit()) {
                    unpack_counts(c[7])
                } else {
                    return None;
                },
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

    /// AN ORDINARY SECOND COSTS ONE BYTE. That is the whole design: a timestamp
    /// and a count is twelve bytes, of which eleven say "and then it was a
    /// second later", so the cadence is the default and only departures from
    /// it are worth encoding.
    #[test]
    fn a_quiet_minute_of_background_costs_a_byte_a_second() {
        let mut pool = Entropy::default();
        let t0 = 1_700_000_000.0;
        for i in 0..60 {
            pool.add_at(t0 + i as f64, (i % 4) as u32);
        }
        let frame = pool.frame(7, false);
        let bytes = frame.encode();
        // 4 magic + 1 flags + seq + started + n + 60 samples + len + 32 key.
        assert_eq!(frame.samples.len(), 60);
        assert!(bytes.len() < 60 + 48, "{} bytes for a minute", bytes.len());
        // And the naive form -- eight bytes of timestamp and four of count --
        // would have been this much worse.
        assert!(bytes.len() * 5 < 60 * 12, "{} vs {}", bytes.len(), 60 * 12);
    }

    /// A frame reads back as the frame that was written, gaps and all.
    #[test]
    fn a_frame_round_trips_through_its_own_encoding() {
        let mut pool = Entropy::default();
        let t0 = 1_700_000_000.0;
        // A quiet stretch, a hole where the counter went away, and a second
        // far too busy for the .tsv's single hex digit.
        for i in 0..10 {
            pool.add_at(t0 + i as f64, i as u32);
        }
        pool.add_at(t0 + 25.0, 9000);
        pool.add_at(t0 + 26.0, 300);
        let frame = pool.frame(3, true);
        let bytes = frame.encode();
        let (back, used) = Frame::decode(&bytes).expect("decodes");
        assert_eq!(used, bytes.len(), "the whole frame was consumed");
        assert_eq!(back, frame);
        assert!(back.suspect);
        // THE COUNTS ARE NOT CLAMPED. The .tsv writes one hex digit a second
        // and stops at fifteen; this is what the tube actually did.
        assert_eq!(back.counts()[10], 9000);
        assert_eq!(back.counts()[11], 300);
        // The hole is recorded as a hole, which the counts alone cannot say:
        // the tenth sample landed at t0+9 and the next at t0+25.
        assert_eq!(back.samples[10].0, 16);
        assert_eq!(back.seconds(), 26);
    }

    /// AND THE FRAME RECOMPUTES ITS OWN KEY. An emission is a claim that
    /// sixty-four characters came out of a particular stretch of decay; the
    /// frame is that stretch, and this is the claim checked against it.
    #[test]
    fn a_frame_recomputes_the_key_it_was_written_with() {
        let mut pool = Entropy::default();
        let t0 = 1_700_000_000.0;
        for i in 0..40 {
            pool.add_at(t0 + i as f64, (i * 7 % 5) as u32);
        }
        // The pool's own sequence number, which is what `draw` will use.
        let frame = pool.frame(pool.seq, false);
        assert!(frame.verifies(), "the frame does not produce its own key");
        // The key it carries is the one the pool goes on to draw.
        let drawn = pool.draw().0;
        assert_eq!(frame.key, drawn);

        // Change one second of it and the key no longer follows.
        let mut tampered = frame.clone();
        tampered.samples[5].1 += 1;
        assert!(!tampered.verifies(), "a changed count still verified");
    }

    /// A FILE CUT SHORT COSTS ONE FRAME, not the rest of the record. The
    /// magic is per frame precisely so a reader that loses its place can scan
    /// forward and carry on.
    #[test]
    fn a_damaged_file_still_yields_the_frames_around_the_damage() {
        let dir = std::env::temp_dir()
            .join(format!("radbeeper-frames-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let mut whole: Vec<Frame> = Vec::new();
        for seq in 0..3u64 {
            let mut pool = Entropy::default();
            let t0 = 1_700_000_000.0 + seq as f64 * 1000.0;
            for i in 0..20 {
                pool.add_at(t0 + i as f64, (seq as u32 + i as u32) % 6);
            }
            let f = pool.frame(seq, false);
            write_frame(&dir, &f, "F48824B8207F7E").unwrap();
            whole.push(f);
        }
        let path = dir.join("random-F48824B8207F7E.bin");
        assert_eq!(read_frames(&path), whole);

        // Corrupt the middle frame's body, leaving the others alone.
        let mut buf = fs::read(&path).unwrap();
        let second = buf
            .windows(4)
            .enumerate()
            .filter(|(_, w)| *w == FRAME_MAGIC)
            .map(|(i, _)| i)
            .nth(1)
            .unwrap();
        buf[second + 8] ^= 0xFF;
        buf[second + 9] ^= 0xFF;
        fs::write(&path, &buf).unwrap();
        let got = read_frames(&path);
        // The first and the last survive whatever the middle one now says.
        assert!(got.contains(&whole[0]), "the frame before the damage was lost");
        assert!(got.contains(&whole[2]), "the frame after the damage was lost");

        // And a file truncated mid-frame yields everything before the cut.
        fs::write(&path, &buf[..buf.len() - 9]).unwrap();
        assert!(read_frames(&path).contains(&whole[0]));
        let _ = fs::remove_dir_all(&dir);
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
