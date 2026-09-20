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
            // Filled in by `write_frame`, which is the only thing that knows
            // what this frame is being appended to.
            link: String::new(),
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
/// `random-<serial>-YYYY-MM.<ext>`, the file a record of that moment belongs in.
///
/// DATED THE SAME WAY THE COUNT LOGS ARE, and for the same reason: rotation
/// by construction, so a month ending is not an event and nothing renames a
/// file while a service is appending to it. It matters more here than there.
/// The count log is a measurement and an old one is merely old; an emission
/// log is an audit trail, and one that grows without bound is one that
/// eventually cannot be published, downloaded or checked. A month is a unit
/// somebody can hold.
pub fn random_path(dir: &Path, serial: &str, ext: &str, when: f64) -> PathBuf {
    dir.join(format!(
        "random-{}-{}.{}",
        if serial.is_empty() { "unknown" } else { serial },
        clock::format(when, "%Y-%m"),
        ext
    ))
}

/// Every emission file of one extension for one counter, oldest first.
///
/// The undated `random-<serial>.<ext>` written before 0.5 sorts FIRST and is
/// read as though it were the oldest month, because it is: it holds
/// everything up to the release that started dating them. Nothing migrates
/// it. A file somebody may have published the hash of is not a file to
/// rewrite, and a reader that handles both costs less than a migration that
/// has to be right the first time.
pub fn random_series(dir: &Path, serial: &str, ext: &str) -> Vec<PathBuf> {
    let serial = if serial.is_empty() { "unknown" } else { serial };
    let head = format!("random-{}", serial);
    let tail = format!(".{}", ext);
    let mut dated: Vec<String> = Vec::new();
    let mut legacy: Option<String> = None;
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    for e in entries.filter_map(|e| e.ok()) {
        let Ok(name) = e.file_name().into_string() else { continue };
        if !name.starts_with(&head) || !name.ends_with(&tail) {
            continue;
        }
        let middle = &name[head.len()..name.len() - tail.len()];
        if middle.is_empty() {
            legacy = Some(name);
        } else if is_dash_month(middle) {
            dated.push(name);
        }
    }
    dated.sort();
    let mut out: Vec<PathBuf> = Vec::new();
    if let Some(n) = legacy {
        out.push(dir.join(n));
    }
    out.extend(dated.into_iter().map(|n| dir.join(n)));
    out
}

/// Whether `s` is exactly `-YYYY-MM`.
///
/// Checked rather than assumed, so that a serial containing a dash cannot
/// make `random-AB-CD.tsv` look like a dated file for counter `AB`.
fn is_dash_month(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 8
        && b[0] == b'-'
        && b[5] == b'-'
        && b[1..5].iter().all(|c| c.is_ascii_digit())
        && b[6..].iter().all(|c| c.is_ascii_digit())
}

/// Where the chain has got to for this counter, across every month it has run.
///
/// Read from the end of the newest frame file at startup, so a service that
/// is restarted -- or a month that has just rolled over -- carries on from
/// the link it left rather than starting a second chain from genesis.
pub fn last_link(dir: &Path, serial: &str) -> String {
    let series = random_series(dir, serial, "bin");
    for path in series.iter().rev() {
        let Some(f) = tail_frame(path) else { continue };
        if !f.link.is_empty() {
            return f.link;
        }
        // A v1 tail. No links were ever written for this counter, so the
        // chain is computed over everything on disk -- once, here, at the
        // moment the first v2 frame is about to be appended.
        let mut at = GENESIS_LINK.to_string();
        for p in &series {
            let mut frames = read_frames(p);
            at = relink(&mut frames, &at);
        }
        return at;
    }
    GENESIS_LINK.to_string()
}

/// The last complete frame in a file, without reading the rest of it.
///
/// Scans BACKWARDS from the end for a magic that decodes and ends exactly at
/// EOF. That last condition is what makes it safe: sample bytes can spell
/// `RBF1` by coincidence -- a second holding 0x52 counts is not absurd -- and
/// a coincidence in the middle of the file will not decode to something that
/// finishes precisely where the file does.
///
/// A file whose final frame was cut short by a full disk has no such
/// position, and this returns None rather than the frame before it: an
/// interrupted append is resumed by appending, and the truncated bytes are
/// left for `read_frames` to scan past.
pub fn tail_frame(path: &Path) -> Option<Frame> {
    let buf = fs::read(path).ok()?;
    if buf.len() < 4 {
        return None;
    }
    let mut at = buf.len() - 4;
    loop {
        if is_magic(&buf, at) {
            if let Some((f, used)) = Frame::decode(&buf[at..]) {
                if at + used == buf.len() {
                    return Some(f);
                }
            }
        }
        if at == 0 {
            return None;
        }
        at -= 1;
    }
}

pub fn write_record(dir: &Path, record: &Record, serial: &str, suspect: bool)
    -> std::io::Result<PathBuf>
{
    let path = random_path(dir, serial, "tsv", record.started);
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
    write_hex(dir, &record.hex, serial, record.started)?;
    Ok(path)
}

/// Append the digits alone to `random-<serial>.hex`: the time it was drawn,
/// two spaces, sixty-four hex digits, one line per emission.
///
/// The .tsv is the audit trail and carries the counts; this is the stream,
/// for anything that just wants the numbers -- `tail -f` it, or cut the
/// second field and feed it on.
pub fn write_hex(dir: &Path, hex: &str, serial: &str, started: f64)
    -> std::io::Result<PathBuf>
{
    // DATED BY WHEN THE POOL OPENED, not by when the line was drawn, so that
    // the three files for one emission always agree about which month it
    // belongs to. A pool that begins at 23:58 on the last of the month and
    // fills at 00:04 on the first would otherwise put its counts in one file
    // and its digits in the next, and a reader pairing them up would find the
    // .hex line with no .tsv row to explain it.
    let path = random_path(dir, serial, "hex", started);
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

/// The same format, plus the chain link. Written by everything from 0.5 on.
///
/// A SECOND MAGIC RATHER THAN A VERSION FIELD, because the magic is what a
/// reader scans for when it has lost its place. A version byte inside the
/// frame would mean a v1 reader finding a v2 frame, parsing its header
/// happily, and running off the end into the next one -- the exact failure
/// `ends_cleanly` exists to catch, arriving by a route it cannot see. Two
/// magics make the wrong version a frame that is simply not there, which is
/// the failure a scanner already handles.
pub const FRAME_MAGIC_V2: &[u8; 4] = b"RBF2";

/// What the chain hangs from: the link before the first frame.
pub const GENESIS_LINK: &str =
    "0000000000000000000000000000000000000000000000000000000000000000";

/// The label under which links are hashed, so a link can never be mistaken
/// for a key even if someone contrives the inputs.
pub const CHAIN_LABEL: &[u8] = b"radbeeper/chain/1";

/// `link(prev, key)` -- one step of the hash chain.
///
/// THIS IS NOT THE RANDOM OUTPUT AND MUST NEVER FEED IT. `digest()` is the
/// emission: label, sequence, start second, counts, and nothing else, byte
/// for byte what the Python computes. Folding the previous link into it would
/// make every published hex line unrecomputable by the reference
/// implementation and would retroactively invalidate every emission already
/// written. So the chain rides ALONGSIDE the keys instead of inside them: it
/// says these keys were emitted in this order by this counter and none has
/// been removed, and it says nothing whatever about the bits.
///
/// Tamper-evidence only. Anyone able to rewrite the file can recompute the
/// whole chain; what they cannot do is rewrite one frame and leave the rest
/// standing, which is the realistic accident -- a truncated copy, a log
/// stitched back together in the wrong order, a frame dropped by a full disk.
pub fn chain(prev: &str, key: &str) -> String {
    let mut h = Sha256::new();
    h.update(CHAIN_LABEL);
    h.update(&hex_bytes(prev));
    h.update(&hex_bytes(key));
    hex(&h.finish())
}

/// Walk a run of frames and give each the link it has earned.
///
/// Frames read from a v1 file carry no link, and frames read from a v2 file
/// carry the one they were written with. This recomputes from `prev`
/// regardless and returns where the chain has got to, so a caller can verify
/// (compare against what is stored) or repair (take what comes back).
pub fn relink(frames: &mut [Frame], prev: &str) -> String {
    let mut at = prev.to_string();
    for f in frames.iter_mut() {
        at = chain(&at, &f.key);
        f.link = at.clone();
    }
    at
}

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
    /// This frame's place in the counter's chain: `H(prev_link || key)`.
    ///
    /// Empty on a frame decoded from a v1 file, which predates the chain --
    /// `relink` fills it in. Never part of the digest; see `chain`.
    pub link: String,
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
        out.extend_from_slice(FRAME_MAGIC_V2);
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
        let link = hex_bytes(&self.link);
        put_varint(&mut out, link.len() as u64);
        out.extend_from_slice(&link);
        out
    }

    /// One frame from the head of `buf`, and how many bytes it used.
    pub fn decode(buf: &[u8]) -> Option<(Frame, usize)> {
        if buf.len() < 5 {
            return None;
        }
        let v2 = match &buf[..4] {
            m if m == FRAME_MAGIC_V2 => true,
            m if m == FRAME_MAGIC => false,
            _ => return None,
        };
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
        // A v1 frame stops at the key. Its link is not absent-because-damaged
        // but absent-because-older, so it is left empty for `relink` rather
        // than guessed at.
        let link = if v2 {
            let llen = get_varint(buf, &mut at)? as usize;
            if llen > buf.len().saturating_sub(at) {
                return None;
            }
            let l = hex(&buf[at..at + llen]);
            at += llen;
            l
        } else {
            String::new()
        };
        Some((Frame { seq, started, suspect, samples, key, link }, at))
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
/// Append a frame to `random-<serial>-YYYY-MM.bin`, linking it to the one before.
///
/// THE LINK IS READ OFF THE DISK RATHER THAN CARRIED IN MEMORY, and that is
/// the whole robustness argument for this design. A service restarts, a month
/// rolls over, a second radbeeper is started by hand against the same
/// directory -- in every one of those cases an in-memory chain head is stale
/// or absent, and the chain silently forks. The file already knows what it
/// ends with, so the file is asked. It costs one backward scan of the tail
/// per emission, which is once every few minutes.
pub fn write_frame(dir: &Path, frame: &Frame, serial: &str) -> std::io::Result<PathBuf> {
    let mut frame = frame.clone();
    frame.link = chain(&last_link(dir, serial), &frame.key);
    let path = random_path(dir, serial, "bin", frame.started as f64);
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
                while at + 4 <= buf.len() && !is_magic(&buf, at) {
                    at += 1;
                }
            }
        }
    }
    out
}

/// Whether `at` is the end of the file or the head of the next frame.
fn ends_cleanly(buf: &[u8], at: usize) -> bool {
    at == buf.len() || is_magic(buf, at)
}

/// Whether a frame of either version starts at `at`.
fn is_magic(buf: &[u8], at: usize) -> bool {
    at + 4 <= buf.len()
        && (&buf[at..at + 4] == FRAME_MAGIC || &buf[at..at + 4] == FRAME_MAGIC_V2)
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
        let minute = pool.frame(7, false);
        assert_eq!(minute.samples.len(), 60);

        // THE MARGINAL COST IS WHAT THE CLAIM IS ABOUT, so it is measured
        // against a longer frame rather than asserted as a total. A total
        // would be a test of the header -- which has grown once already, for
        // the chain link -- dressed up as a test of the encoding.
        let span = |n: usize| {
            let mut p = Entropy::default();
            for i in 0..n {
                p.add_at(t0 + i as f64, (i % 4) as u32);
            }
            p.frame(7, false).encode().len()
        };
        // BOTH LENGTHS ARE OVER 127 ON PURPOSE. The sample count is a varint,
        // so 60 costs one byte and 600 costs two, and measuring across that
        // boundary charges the encoding a byte that belongs to the header.
        let grew = span(670) - span(130);
        assert_eq!(grew, 540, "540 more seconds should cost 540 more bytes");

        // And the naive form -- eight bytes of timestamp and four of count --
        // would have been this much worse.
        let bytes = minute.encode().len();
        assert!(bytes * 4 < 60 * 12, "{} vs {}", bytes, 60 * 12);
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
        // The frames as they went in carry no link; the writer chains them as
        // it appends, so that is what should come back out.
        relink(&mut whole, GENESIS_LINK);
        let series = random_series(&dir, "F48824B8207F7E", "bin");
        assert_eq!(series.len(), 1, "one month, one file: {:?}", series);
        let path = series[0].clone();
        assert_eq!(read_frames(&path), whole);

        // Corrupt the middle frame's body, leaving the others alone.
        let mut buf = fs::read(&path).unwrap();
        let second = buf
            .windows(4)
            .enumerate()
            .filter(|(_, w)| *w == FRAME_MAGIC_V2)
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

    /// A temporary directory of this test's own, removed on the way out.
    fn scratch(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("radbeeper-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A pool that has counted `n` seconds from `t0`.
    fn pool_of(t0: f64, n: usize, seed: u32) -> Entropy {
        let mut p = Entropy::default();
        for i in 0..n {
            p.add_at(t0 + i as f64, (seed + i as u32) % 6);
        }
        p
    }

    /// THE CHAIN MUST NOT DEPEND ON THE PROCESS THAT WROTE IT. A service is
    /// restarted, and the frame it writes afterwards has to hang off the one
    /// before it -- which is on disk and nowhere else. This is the reason
    /// `write_frame` reads the tail instead of carrying a chain head in
    /// memory, so it is the reason stated as a test.
    #[test]
    fn the_chain_carries_on_across_a_restart_and_a_month_boundary() {
        let dir = scratch("chain");
        let serial = "F48824B8207F7E";
        // Mid-November and mid-December 2023. A MONTH APART RATHER THAN AN
        // HOUR, because the file name is formatted in local time and a pair
        // that straddles midnight UTC lands in one month or two depending on
        // the zone the test happens to run in.
        for (seq, t0) in [(0u64, 1_700_000_000.0), (1, 1_702_592_000.0)] {
            // A fresh pool each time, exactly as a restarted service has.
            let f = pool_of(t0, 20, seq as u32).frame(seq, false);
            write_frame(&dir, &f, serial).unwrap();
        }
        let series = random_series(&dir, serial, "bin");
        assert_eq!(series.len(), 2, "two months, two files: {:?}", series);

        let all: Vec<Frame> =
            series.iter().flat_map(|p| read_frames(p)).collect();
        assert_eq!(all.len(), 2);
        let mut want = all.clone();
        relink(&mut want, GENESIS_LINK);
        assert_eq!(all[0].link, want[0].link, "the first link is not genesis-based");
        assert_eq!(
            all[1].link,
            chain(&all[0].link, &all[1].key),
            "December did not hang off November"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// Dropping a frame out of the middle is what the chain exists to catch.
    /// Every frame still verifies against its own counts -- that is the point:
    /// per-frame integrity cannot see a deletion, and the chain can.
    #[test]
    fn a_missing_frame_breaks_the_chain_though_every_frame_still_verifies() {
        let mut frames: Vec<Frame> = (0..4u64)
            .map(|seq| pool_of(1_700_000_000.0 + seq as f64 * 100.0, 15, seq as u32)
                .frame(seq, false))
            .collect();
        relink(&mut frames, GENESIS_LINK);
        assert!(frames.iter().all(|f| f.verifies()));

        let kept: Vec<Frame> =
            frames.iter().enumerate().filter(|(i, _)| *i != 2)
                  .map(|(_, f)| f.clone()).collect();
        // Still individually sound, and that is exactly the blind spot.
        assert!(kept.iter().all(|f| f.verifies()));
        // The chain notices, because frame 3's link names frame 2's.
        let mut recomputed = kept.clone();
        relink(&mut recomputed, GENESIS_LINK);
        assert_ne!(
            recomputed.last().unwrap().link,
            kept.last().unwrap().link,
            "a deletion went unnoticed"
        );
    }

    /// The chain rides alongside the key and never inside it, so an emission
    /// written before 0.5 recomputes exactly as it always did.
    #[test]
    fn linking_a_frame_does_not_touch_the_key_it_carries() {
        let pool = pool_of(1_700_000_000.0, 30, 3);
        let bare = pool.frame(9, false);
        let mut linked = vec![bare.clone()];
        relink(&mut linked, GENESIS_LINK);
        assert_eq!(bare.key, linked[0].key, "linking changed the emission");
        assert!(linked[0].verifies());
        assert!(bare.link.is_empty(), "a frame is born unlinked");
        assert_ne!(linked[0].link, linked[0].key, "the link is not the key");
    }

    /// A v1 file predates the chain, so it reads back with no link rather
    /// than with a guessed one -- and `relink` is what supplies it.
    #[test]
    fn a_v1_frame_reads_back_unlinked_and_can_be_linked_afterwards() {
        let f = pool_of(1_700_000_000.0, 12, 1).frame(4, false);
        // A v1 frame is a v2 frame without the trailing link, which is what
        // the old encoder wrote: same body, same key, stopping at the key.
        // A frame is born unlinked, and an unlinked frame encodes its link
        // as a zero length -- so it has to be linked before there are 33
        // bytes on the end to take off.
        let mut linked = vec![f.clone()];
        relink(&mut linked, GENESIS_LINK);
        let v2 = linked[0].encode();
        let cut = v2.len() - 33;
        let mut v1 = Vec::from(&v2[..cut]);
        v1[..4].copy_from_slice(FRAME_MAGIC);

        let (got, used) = Frame::decode(&v1).expect("a v1 frame should decode");
        assert_eq!(used, v1.len());
        assert_eq!(got.key, f.key);
        assert!(got.link.is_empty());
        assert!(got.verifies(), "a v1 frame still recomputes its own key");

        let mut one = vec![got];
        relink(&mut one, GENESIS_LINK);
        assert_eq!(one[0].link, chain(GENESIS_LINK, &f.key));
    }

    /// The undated file written before 0.5 is read as the oldest month, and a
    /// serial with a dash in it does not masquerade as a date.
    #[test]
    fn the_series_puts_the_undated_file_first_and_is_not_fooled_by_a_dash() {
        let dir = scratch("series");
        for name in [
            "random-A-B.bin",           // serial "A-B", no date
            "random-A-B-2026-01.bin",   // serial "A-B", January
            "random-A-B-2025-12.bin",   // serial "A-B", the December before
            "random-A-B-notamonth.bin", // not a date, not this counter's
            "random-OTHER-2026-01.bin",
        ] {
            fs::write(dir.join(name), b"").unwrap();
        }
        let got: Vec<String> = random_series(&dir, "A-B", "bin")
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            got,
            vec![
                "random-A-B.bin".to_string(),
                "random-A-B-2025-12.bin".to_string(),
                "random-A-B-2026-01.bin".to_string(),
            ]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    /// All three files for one emission carry the same month.
    ///
    /// The pool opens at 23:58 on the last day of a month and fills six
    /// minutes later, in the next one. The .tsv row, the .hex line and the
    /// .bin frame all belong to the month it OPENED in, because a reader
    /// pairing them up by month should never find one without the others.
    #[test]
    fn an_emission_that_spans_midnight_lands_in_one_month_not_two() {
        let dir = scratch("midnight");
        let serial = "A1";
        // 2023-11-30 23:58 local, wherever this test runs.
        let open_at = {
            let mut t = 1_701_388_680.0;
            // Nudge to 23:58 local on the last of the month by asking the
            // formatter rather than assuming a zone.
            for _ in 0..48 {
                if clock::format(t, "%d") == "30" && clock::format(t, "%H") == "23" {
                    break;
                }
                t += 1800.0;
            }
            t
        };
        let pool = pool_of(open_at, 20, 1);
        let record = Record {
            seq: 0,
            started: open_at,
            seconds: 20,
            bits: 256.0,
            rate: pool.rate(),
            counts: pack_counts(&pool.counts),
            hex: pool.digest(0),
        };
        write_record(&dir, &record, serial, false).unwrap();
        write_frame(&dir, &pool.frame(0, false), serial).unwrap();

        let month = |ext: &str| -> Vec<String> {
            random_series(&dir, serial, ext)
                .iter()
                .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
                .collect()
        };
        let tsv = month("tsv");
        assert_eq!(tsv.len(), 1, "{:?}", tsv);
        assert_eq!(month("hex"), tsv.iter().map(|n| n.replace(".tsv", ".hex"))
                                    .collect::<Vec<_>>());
        assert_eq!(month("bin"), tsv.iter().map(|n| n.replace(".tsv", ".bin"))
                                    .collect::<Vec<_>>());
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
