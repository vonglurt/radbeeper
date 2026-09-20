// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// THE FRAMES API: raw decay data out of a log directory, and back into one.
//
// The pieces have all existed since 0.5 -- `entropy` owns the encoding, the
// chain and the files; `analysis` owns the spectrum -- but they existed as
// the internals of a program rather than as a surface something else could
// call. This module is that surface and nothing else: it holds no format of
// its own, it decides nothing `entropy` has already decided, and every byte
// it returns came off a file written by the service.
//
// WHAT THE SHAPE IS, AND WHY IT IS THIS ONE. A counter's raw record is a
// series of month files, appended to forever and never rewritten. So the
// surface is a `Series` you open on a directory, the months it has, and the
// frames in a month -- which is the same nesting the browser navigates and
// the same nesting the files are already in. Nothing here builds an index,
// because an index is a second copy of the truth and this format's whole
// argument is that there is only ever one.
//
// IN IS NARROWER THAN OUT, ON PURPOSE. Data comes out as bytes, as TSV and
// as JSON. It goes back in as bytes or as JSON, and either way every frame
// is recomputed from its own counts before it is appended -- a frame that
// cannot produce the key it carries is not written, whatever it says about
// itself. See `import`.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::analysis::Ladder;
use crate::clock;
use crate::entropy::{self, Frame, GENESIS_LINK};

/// The `-YYYY-MM` suffix a frame file carries, or `""` for the undated file
/// written before 0.5 rotated them.
pub type MonthLabel = String;

/// One month's frame file for one counter.
pub struct Month {
    /// `2026-09`, or `""` for the legacy undated file.
    pub label: MonthLabel,
    pub path: PathBuf,
    /// The file's size, which is not the same as the frames that decode from
    /// it: a truncated tail costs one frame and leaves its bytes on disk.
    pub bytes: u64,
    pub frames: usize,
    /// `started` of the first and last frame that decoded, in epoch seconds.
    pub first: i64,
    pub last: i64,
}

/// Every month file one counter has written into one directory.
///
/// Opening reads them: a frame is about 450 bytes and a busy month is a
/// couple of megabytes, so the alternative -- a lazy handle that has to be
/// asked twice for the same answer -- costs more in surface than it saves in
/// reads. `bytes()` is the cheap question and it is answered from the
/// metadata; `frames()` is the true one and it is answered from the data.
pub struct Series {
    dir: PathBuf,
    serial: String,
    months: Vec<Month>,
}

impl Series {
    /// The frame files for `serial` under `dir`, oldest month first.
    pub fn open(dir: &Path, serial: &str) -> Series {
        let mut months = Vec::new();
        for path in entropy::random_series(dir, serial, "bin") {
            let bytes = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            let frames = entropy::read_frames(&path);
            months.push(Month {
                label: month_label(&path, serial),
                bytes,
                frames: frames.len(),
                first: frames.first().map(|f| f.started).unwrap_or(0),
                last: frames.last().map(|f| f.started).unwrap_or(0),
                path,
            });
        }
        Series { dir: dir.to_path_buf(), serial: serial.to_string(), months }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    /// The months, oldest first. The legacy undated file, if there is one,
    /// sorts ahead of every dated one -- it is older than the rotation.
    pub fn months(&self) -> &[Month] {
        &self.months
    }

    pub fn month(&self, label: &str) -> Option<&Month> {
        self.months.iter().find(|m| m.label == label)
    }

    /// Every frame in one month, in the order it was written.
    pub fn read(&self, label: &str) -> Vec<Frame> {
        match self.month(label) {
            Some(m) => entropy::read_frames(&m.path),
            None => Vec::new(),
        }
    }

    /// Every frame this counter has, oldest month first.
    ///
    /// THIS IS THE ORDER THE CHAIN IS IN. `seq` restarts -- it belongs to the
    /// pool, not to the record -- so file order is the only order that means
    /// anything, and `verify` reads it in exactly this sequence.
    pub fn read_all(&self) -> Vec<Frame> {
        let mut out = Vec::new();
        for m in &self.months {
            out.extend(entropy::read_frames(&m.path));
        }
        out
    }

    /// Bytes on disk across every month.
    pub fn bytes(&self) -> u64 {
        self.months.iter().map(|m| m.bytes).sum()
    }

    /// Frames that decode, across every month.
    pub fn frames(&self) -> usize {
        self.months.iter().map(|m| m.frames).sum()
    }
}

/// `2026-09` out of `random-F488-2026-09.bin`, or `""` when it is undated.
fn month_label(path: &Path, serial: &str) -> MonthLabel {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let head = format!("random-{}", if serial.is_empty() { "unknown" } else { serial });
    let middle = name.strip_prefix(&head).and_then(|r| r.strip_suffix(".bin"));
    match middle {
        Some(m) if m.len() == 8 && m.starts_with('-') => m[1..].to_string(),
        _ => String::new(),
    }
}

/// Every counter with frames in `dir`, by serial.
///
/// Read off the filenames rather than the contents: a frame does not carry
/// the serial of the counter that drew it, because the file it is in does,
/// and putting it in both is how the two come to disagree.
pub fn counters(dir: &Path) -> Vec<String> {
    let mut out: BTreeSet<String> = BTreeSet::new();
    let Ok(entries) = fs::read_dir(dir) else { return Vec::new() };
    for e in entries.filter_map(|e| e.ok()) {
        let Ok(name) = e.file_name().into_string() else { continue };
        let Some(rest) = name.strip_prefix("random-") else { continue };
        let Some(stem) = rest.strip_suffix(".bin") else { continue };
        if stem.is_empty() {
            continue;
        }
        // `random-A-B-2026-01.bin` is counter `A-B` in January, and
        // `random-A-B.bin` is counter `A-B` undated. A serial may contain a
        // dash, so the month is only taken off when it really is one.
        let serial = match stem.len() > 8 && is_month_suffix(&stem[stem.len() - 8..]) {
            true => &stem[..stem.len() - 8],
            false => stem,
        };
        if !serial.is_empty() {
            out.insert(serial.to_string());
        }
    }
    out.into_iter().collect()
}

fn is_month_suffix(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 8
        && b[0] == b'-'
        && b[5] == b'-'
        && b[1..5].iter().all(|c| c.is_ascii_digit())
        && b[6..].iter().all(|c| c.is_ascii_digit())
}

/// Frames grouped by the local day they started in, oldest first.
///
/// The values are indices into `frames`, not copies: a day's worth of frames
/// is a few hundred kilobytes and the caller already owns them.
pub fn by_day(frames: &[Frame]) -> Vec<(String, Vec<usize>)> {
    let mut out: Vec<(String, Vec<usize>)> = Vec::new();
    for (i, f) in frames.iter().enumerate() {
        let day = clock::format(f.started as f64, "%Y-%m-%d");
        match out.last_mut() {
            Some((d, v)) if *d == day => v.push(i),
            _ => out.push((day, vec![i])),
        }
    }
    out
}

/// What the spectrum says about one frame's seconds.
///
/// THE SAME ARITHMETIC AS THE FLAG, OVER DIFFERENT SECONDS -- and the
/// difference is the point rather than a defect.
///
/// `suspect` is written when the frame is drawn, out of the monitor's
/// RUNNING ladder: every second that process has seen, summed across the
/// tubes when there are two. This is the ladder over THIS FRAME's samples
/// and nothing else. A 363-second frame gives four 128-second windows; the
/// recorder may have had a hundred, and a period an hour long is invisible
/// to the frame and obvious to the run.
///
/// So the two answer different questions -- "was the room periodic while
/// this was drawn" and "is this frame periodic" -- and they are allowed to
/// disagree. `verify` does not compare them, because there is no wrong
/// answer to report.
pub struct Spec {
    /// Seconds per window on the rung that was used.
    pub window: usize,
    /// Each bin against the average bin: 1.0 is what flat looks like.
    pub relative: Vec<f64>,
    /// The loudest bin's height and its index.
    pub loudest: (f64, usize),
    /// How high a bin can get by luck alone, for this many windows.
    pub chance_max: f64,
    /// The period of the loudest bin, in seconds.
    pub period: f64,
    /// Whether the recorder would have called this frame suspect.
    pub suspect: bool,
    /// Windows averaged. Zero means the frame was too short to say anything.
    pub runs: u32,
}

/// The spectrum of one frame's counts, or `None` when it is too short.
pub fn spectrum(counts: &[u32]) -> Option<Spec> {
    let mut ladder = Ladder::new();
    for c in counts {
        ladder.add(*c);
    }
    let spec = ladder.best();
    if spec.runs == 0 {
        return None;
    }
    let loudest = spec.loudest();
    let chance_max = spec.chance_max();
    Some(Spec {
        window: spec.window,
        relative: spec.relative(),
        loudest,
        chance_max,
        period: spec.period(loudest.1),
        // The recorder's rule, character for character: main.rs draws it as
        // `top > 0.0 && top >= spec.chance_max() * 1.25`.
        suspect: loudest.0 > 0.0 && loudest.0 >= chance_max * 1.25,
        runs: spec.runs,
    })
}

/// What `verify` found.
pub struct Verdict {
    pub frames: usize,
    /// Frames whose counts really do produce the key written in them.
    pub keys_ok: usize,
    /// Frames whose link is `H(label ‖ previous link ‖ key)`.
    pub links_ok: usize,
    /// Frames carrying no link at all -- v1, written before the chain.
    pub unlinked: usize,
    /// The `seq` of every frame that failed either test, in file order.
    pub broken: Vec<u64>,
    /// Where the chain ends, which is what the next frame will link to.
    pub head: String,
}

/// Recompute every key from its own counts, and every link from the one
/// before it.
///
/// `prev` is the link the first frame should follow: `GENESIS_LINK` for a
/// whole series, or the head of whatever came before for a fragment.
pub fn verify(frames: &[Frame], prev: &str) -> Verdict {
    let mut v = Verdict {
        frames: frames.len(),
        keys_ok: 0,
        links_ok: 0,
        unlinked: 0,
        broken: Vec::new(),
        head: prev.to_string(),
    };
    for f in frames {
        let key_ok = f.verifies();
        if key_ok {
            v.keys_ok += 1;
        }
        let want = entropy::chain(&v.head, &f.key);
        let link_ok = if f.link.is_empty() {
            // A v1 frame predates the chain. It is not a break -- there was
            // nothing to break yet -- but the chain cannot run through it
            // either, so the head advances as though it had been linked.
            v.unlinked += 1;
            true
        } else if f.link == want {
            v.links_ok += 1;
            true
        } else {
            false
        };
        if !key_ok || !link_ok {
            v.broken.push(f.seq);
        }
        v.head = want;
    }
    v
}

/// Where the chain has got to for this counter, across every month.
pub fn head(dir: &Path, serial: &str) -> String {
    let link = entropy::last_link(dir, serial);
    if link.is_empty() { GENESIS_LINK.to_string() } else { link }
}

// ------------------------------------------------------------------ out ---

/// Frames as JSON: an array of objects, one per frame.
///
/// THE FIELD NAMES ARE THE STRUCT'S FIELD NAMES and the samples stay pairs,
/// so what comes out is the frame rather than a rendering of it --
/// `tests/fixtures/frames.json` is this shape and `from_json` reads it back.
pub fn to_json(frames: &[Frame]) -> String {
    let mut out = String::from("[\n");
    for (i, f) in frames.iter().enumerate() {
        if i > 0 {
            out.push_str(",\n");
        }
        out.push_str("  {");
        out.push_str(&format!("\"seq\": {}, ", f.seq));
        out.push_str(&format!("\"started\": {}, ", f.started));
        out.push_str(&format!("\"suspect\": {}, ", f.suspect));
        out.push_str(&format!("\"key\": \"{}\", ", f.key));
        out.push_str(&format!("\"link\": \"{}\", ", f.link));
        out.push_str("\"samples\": [");
        for (k, (gap, count)) in f.samples.iter().enumerate() {
            if k > 0 {
                out.push_str(", ");
            }
            out.push_str(&format!("[{}, {}]", gap, count));
        }
        out.push_str("]}");
    }
    out.push_str("\n]\n");
    out
}

/// Frames as the tab-separated table the audit page already publishes.
///
/// One row a frame, and the columns `random --frames` prints. `head` is
/// written as `#` lines ahead of the header, for whatever the caller wants
/// to say about where the rows came from.
pub fn to_tsv(frames: &[Frame], head: &[String]) -> String {
    let mut out = String::new();
    for line in head {
        out.push_str(&format!("# {}\n", line));
    }
    out.push_str("#seq\tstarted\tsamples\tcounts\tseconds\tsuspect\tkey\tlink\n");
    for f in frames {
        let counts: u64 = f.counts().iter().map(|c| *c as u64).sum();
        out.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            f.seq,
            clock::stamp(f.started as f64),
            f.samples.len(),
            counts,
            f.seconds(),
            if f.suspect { "yes" } else { "no" },
            f.key,
            f.link
        ));
    }
    out
}

/// The exact bytes of the frames, as the file holds them.
///
/// This is the canonical form and the only lossless one: `to_json` and
/// `to_tsv` are views, and a round trip through either is a round trip
/// through a reader that was written for people.
pub fn to_bytes(frames: &[Frame]) -> Vec<u8> {
    let mut out = Vec::new();
    for f in frames {
        out.extend_from_slice(&f.encode());
    }
    out
}

// ------------------------------------------------------------------- in ---

/// Frames out of the JSON `to_json` writes.
///
/// Strict on the shape and forgiving about whitespace. A field that is
/// missing, a sample that is not a pair, a key that is not hex: all of them
/// are an error naming the frame, rather than a frame with a zero in it.
pub fn from_json(text: &str) -> Result<Vec<Frame>, String> {
    let b: Vec<char> = text.chars().collect();
    let mut at = 0usize;
    skip_ws(&b, &mut at);
    expect(&b, &mut at, '[')?;
    let mut out = Vec::new();
    skip_ws(&b, &mut at);
    if peek(&b, at) == Some(']') {
        at += 1;
    } else {
        loop {
            skip_ws(&b, &mut at);
            out.push(frame_from_json(&b, &mut at, out.len())?);
            skip_ws(&b, &mut at);
            match peek(&b, at) {
                Some(',') => at += 1,
                Some(']') => {
                    at += 1;
                    break;
                }
                _ => return Err(format!("frame {}: expected , or ] at {}",
                                        out.len() - 1, at)),
            }
        }
    }
    // NOTHING MAY FOLLOW THE ARRAY. Two JSON documents concatenated is the
    // shape a half-finished transfer takes, and a reader that stops at the
    // first `]` would import the first half and call it the file.
    skip_ws(&b, &mut at);
    match peek(&b, at) {
        None => Ok(out),
        Some(c) => Err(format!("{:?} after the frames, at {}", c, at)),
    }
}

fn frame_from_json(b: &[char], at: &mut usize, n: usize) -> Result<Frame, String> {
    let where_ = |m: &str| format!("frame {}: {}", n, m);
    expect(b, at, '{').map_err(|e| where_(&e))?;
    let mut seq = None;
    let mut started = None;
    let mut suspect = None;
    let mut key: Option<String> = None;
    let mut link: Option<String> = None;
    let mut samples: Option<Vec<(u32, u32)>> = None;
    loop {
        skip_ws(b, at);
        if peek(b, *at) == Some('}') {
            *at += 1;
            break;
        }
        let name = string_from_json(b, at).map_err(|e| where_(&e))?;
        skip_ws(b, at);
        expect(b, at, ':').map_err(|e| where_(&e))?;
        skip_ws(b, at);
        match name.as_str() {
            "seq" => seq = Some(number_from_json(b, at).map_err(|e| where_(&e))? as u64),
            "started" => started = Some(number_from_json(b, at).map_err(|e| where_(&e))?),
            "suspect" => suspect = Some(bool_from_json(b, at).map_err(|e| where_(&e))?),
            "key" => key = Some(string_from_json(b, at).map_err(|e| where_(&e))?),
            "link" => link = Some(string_from_json(b, at).map_err(|e| where_(&e))?),
            "samples" => samples = Some(samples_from_json(b, at).map_err(|e| where_(&e))?),
            other => return Err(where_(&format!("unknown field {:?}", other))),
        }
        skip_ws(b, at);
        match peek(b, *at) {
            Some(',') => *at += 1,
            Some('}') => {
                *at += 1;
                break;
            }
            _ => return Err(where_("expected , or }")),
        }
    }
    let key = key.ok_or_else(|| where_("no key"))?;
    let link = link.unwrap_or_default();
    for (what, hex) in [("key", &key), ("link", &link)] {
        if !hex.is_empty() && (hex.len() != 64 || !hex.chars().all(|c| c.is_ascii_hexdigit())) {
            return Err(where_(&format!("{} is not 64 hex characters", what)));
        }
    }
    Ok(Frame {
        seq: seq.ok_or_else(|| where_("no seq"))?,
        started: started.ok_or_else(|| where_("no started"))?,
        suspect: suspect.ok_or_else(|| where_("no suspect"))?,
        samples: samples.ok_or_else(|| where_("no samples"))?,
        key,
        link,
    })
}

fn samples_from_json(b: &[char], at: &mut usize) -> Result<Vec<(u32, u32)>, String> {
    expect(b, at, '[')?;
    let mut out = Vec::new();
    skip_ws(b, at);
    if peek(b, *at) == Some(']') {
        *at += 1;
        return Ok(out);
    }
    loop {
        skip_ws(b, at);
        expect(b, at, '[')?;
        skip_ws(b, at);
        let gap = number_from_json(b, at)?;
        skip_ws(b, at);
        expect(b, at, ',')?;
        skip_ws(b, at);
        let count = number_from_json(b, at)?;
        skip_ws(b, at);
        expect(b, at, ']')?;
        if gap < 0 || count < 0 {
            return Err("a sample cannot be negative".to_string());
        }
        if gap > u32::MAX as i64 || count > u32::MAX as i64 {
            return Err("a sample does not fit in 32 bits".to_string());
        }
        out.push((gap as u32, count as u32));
        skip_ws(b, at);
        match peek(b, *at) {
            Some(',') => *at += 1,
            Some(']') => {
                *at += 1;
                break;
            }
            _ => return Err("expected , or ] in samples".to_string()),
        }
    }
    Ok(out)
}

fn skip_ws(b: &[char], at: &mut usize) {
    while matches!(peek(b, *at), Some(c) if c.is_whitespace()) {
        *at += 1;
    }
}

fn peek(b: &[char], at: usize) -> Option<char> {
    b.get(at).copied()
}

fn expect(b: &[char], at: &mut usize, want: char) -> Result<(), String> {
    match peek(b, *at) {
        Some(c) if c == want => {
            *at += 1;
            Ok(())
        }
        Some(c) => Err(format!("expected {:?} at {}, found {:?}", want, at, c)),
        None => Err(format!("expected {:?}, found the end", want)),
    }
}

fn string_from_json(b: &[char], at: &mut usize) -> Result<String, String> {
    expect(b, at, '"')?;
    let mut out = String::new();
    loop {
        match peek(b, *at) {
            None => return Err("a string that never ends".to_string()),
            Some('"') => {
                *at += 1;
                return Ok(out);
            }
            Some('\\') => {
                *at += 1;
                let c = peek(b, *at).ok_or("an escape that never ends")?;
                *at += 1;
                out.push(match c {
                    'n' => '\n',
                    't' => '\t',
                    'r' => '\r',
                    'b' => '\u{8}',
                    'f' => '\u{c}',
                    'u' => {
                        let mut v = 0u32;
                        for _ in 0..4 {
                            let d = peek(b, *at).ok_or("a short \\u escape")?;
                            v = v * 16 + d.to_digit(16).ok_or("a bad \\u escape")?;
                            *at += 1;
                        }
                        char::from_u32(v).ok_or("a \\u escape that is not a character")?
                    }
                    c => c,
                });
            }
            Some(c) => {
                *at += 1;
                out.push(c);
            }
        }
    }
}

fn number_from_json(b: &[char], at: &mut usize) -> Result<i64, String> {
    let start = *at;
    if peek(b, *at) == Some('-') {
        *at += 1;
    }
    while matches!(peek(b, *at), Some(c) if c.is_ascii_digit()) {
        *at += 1;
    }
    if *at == start {
        return Err(format!("expected a number at {}", start));
    }
    let text: String = b[start..*at].iter().collect();
    text.parse::<i64>().map_err(|_| format!("{:?} is not a whole number", text))
}

fn bool_from_json(b: &[char], at: &mut usize) -> Result<bool, String> {
    for (word, value) in [("true", true), ("false", false)] {
        let end = *at + word.len();
        if end <= b.len() && b[*at..end].iter().collect::<String>() == word {
            *at = end;
            return Ok(value);
        }
    }
    Err(format!("expected true or false at {}", at))
}

/// What `import` did.
pub struct Imported {
    pub offered: usize,
    /// Written, and where.
    pub written: usize,
    pub files: Vec<PathBuf>,
    /// Already on disk under the same key, so not written again.
    pub duplicates: usize,
    /// Could not produce the key they carry, so not written at all.
    pub refused: Vec<u64>,
}

/// Verify frames and append the ones that hold up to `serial`'s month files.
///
/// THREE RULES, AND THEY ARE THE WHOLE PROCEDURE.
///
/// 1. **A frame must recompute.** Its counts are hashed and the result has to
///    be the key it carries. This is the only claim a frame makes and the
///    only one that can be checked without trusting whoever sent it.
/// 2. **A key already on disk is not written twice.** Importing the same file
///    twice leaves the record as it was, so a re-run after a half-finished
///    transfer is safe.
/// 3. **The link is recomputed here, never carried in.** A chain is a
///    property of the order frames landed in THIS directory. Keeping a
///    sender's links would either fork the chain or silently claim their
///    history happened here; `entropy::write_frame` reads the head off the
///    disk and links to that.
pub fn import(dir: &Path, serial: &str, frames: &[Frame]) -> io::Result<Imported> {
    let mut out = Imported {
        offered: frames.len(),
        written: 0,
        files: Vec::new(),
        duplicates: 0,
        refused: Vec::new(),
    };
    let have: BTreeSet<String> =
        Series::open(dir, serial).read_all().into_iter().map(|f| f.key).collect();
    let mut seen = have;
    for f in frames {
        if !f.verifies() {
            out.refused.push(f.seq);
            continue;
        }
        if !seen.insert(f.key.clone()) {
            out.duplicates += 1;
            continue;
        }
        let path = entropy::write_frame(dir, f, serial)?;
        out.written += 1;
        if !out.files.contains(&path) {
            out.files.push(path);
        }
    }
    Ok(out)
}

/// Frames out of a file, whichever of the two forms it is in.
///
/// The bytes are the canonical form, so they are tried first and a file that
/// starts with a frame magic is never parsed as anything else.
pub fn read_any(path: &Path) -> Result<Vec<Frame>, String> {
    let bytes = fs::read(path).map_err(|e| format!("{}: {}", path.display(), e))?;
    if bytes.starts_with(entropy::FRAME_MAGIC) || bytes.starts_with(entropy::FRAME_MAGIC_V2) {
        let frames = entropy::read_frames(path);
        if frames.is_empty() {
            return Err(format!("{}: frame bytes that do not decode", path.display()));
        }
        return Ok(frames);
    }
    let text = String::from_utf8(bytes)
        .map_err(|_| format!("{}: neither frame bytes nor text", path.display()))?;
    from_json(&text).map_err(|e| format!("{}: {}", path.display(), e))
}

// ----------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(name)
    }

    fn tempdir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("radbeeper-frames-{}-{}", tag, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// THE JSON IS THE FRAME, NOT A RENDERING OF IT. The fixture the wire
    /// format is tested against parses with this reader, and what it parses
    /// to is what the bytes beside it decode to.
    #[test]
    fn the_json_fixture_and_the_byte_fixture_are_the_same_frames() {
        let from_bytes = entropy::read_frames(&fixture("frames.bin"));
        let text = fs::read_to_string(fixture("frames.json")).unwrap();
        let from_text = from_json(&text).unwrap();
        assert_eq!(from_bytes.len(), from_text.len());
        assert!(!from_bytes.is_empty(), "the fixture has frames");
        for (a, b) in from_bytes.iter().zip(&from_text) {
            assert_eq!(a, b);
        }
    }

    /// Out and back in, through the reader people will actually use.
    #[test]
    fn json_round_trips_every_field() {
        let frames = entropy::read_frames(&fixture("frames.bin"));
        let back = from_json(&to_json(&frames)).unwrap();
        assert_eq!(frames, back);
    }

    /// The bytes are the canonical form, so they have to survive exactly.
    #[test]
    fn bytes_round_trip_byte_for_byte() {
        let want = fs::read(fixture("frames.bin")).unwrap();
        let frames = entropy::read_frames(&fixture("frames.bin"));
        assert_eq!(to_bytes(&frames), want);
    }

    /// A frame that does not produce its own key is refused, and refusing it
    /// does not stop the frames around it from landing.
    #[test]
    fn an_invented_frame_is_refused_and_the_rest_still_land() {
        let dir = tempdir("refuse");
        let mut frames = entropy::read_frames(&fixture("frames.bin"));
        assert!(frames.len() >= 2);
        let liar = frames.len() - 1;
        frames[liar].key = "ff".repeat(32);
        let done = import(&dir, "A1", &frames).unwrap();
        assert_eq!(done.written, frames.len() - 1);
        assert_eq!(done.refused, vec![frames[liar].seq]);
        assert_eq!(Series::open(&dir, "A1").frames(), frames.len() - 1);
    }

    /// Rule two: importing the same file twice is not two copies of it.
    #[test]
    fn importing_twice_writes_once() {
        let dir = tempdir("dupe");
        let frames = entropy::read_frames(&fixture("frames.bin"));
        let first = import(&dir, "A1", &frames).unwrap();
        let second = import(&dir, "A1", &frames).unwrap();
        assert_eq!(first.written, frames.len());
        assert_eq!(second.written, 0);
        assert_eq!(second.duplicates, frames.len());
        assert_eq!(Series::open(&dir, "A1").frames(), frames.len());
    }

    /// Rule three: the chain is this directory's, and it verifies from
    /// genesis after an import from somewhere else.
    #[test]
    fn an_import_is_relinked_into_this_directorys_chain() {
        let dir = tempdir("chain");
        let mut frames = entropy::read_frames(&fixture("frames.bin"));
        for f in frames.iter_mut() {
            f.link = "ab".repeat(32); // somebody else's chain
        }
        import(&dir, "A1", &frames).unwrap();
        let landed = Series::open(&dir, "A1").read_all();
        let v = verify(&landed, GENESIS_LINK);
        assert_eq!(v.keys_ok, landed.len());
        assert_eq!(v.links_ok, landed.len());
        assert!(v.broken.is_empty());
        assert_eq!(v.head, head(&dir, "A1"));
    }

    /// A break in the middle is named, and only the frames it touches.
    #[test]
    fn verify_names_the_frame_that_broke() {
        let mut frames = entropy::read_frames(&fixture("frames.bin"));
        let head_link = GENESIS_LINK.to_string();
        let good = verify(&frames, &head_link);
        assert!(good.broken.is_empty(), "the fixture verifies as it stands");
        let victim = 1.min(frames.len() - 1);
        frames[victim].link = "cd".repeat(32);
        let bad = verify(&frames, &head_link);
        assert_eq!(bad.broken, vec![frames[victim].seq]);
        assert_eq!(bad.keys_ok, frames.len());
    }

    /// Months come back oldest first, and the labels are the months.
    #[test]
    fn a_series_is_its_months_in_order() {
        let dir = tempdir("months");
        let frames = entropy::read_frames(&fixture("frames.bin"));
        // One frame in January, the rest where the fixture put them. The
        // key is left as it is: `Series` reads what is on disk and does not
        // check it, and the checking is `verify`'s test, above.
        let mut january = frames[0].clone();
        january.started = 1_704_067_200; // 2024-01-01T00:00:00Z
        entropy::write_frame(&dir, &january, "A1").unwrap();
        for f in &frames {
            entropy::write_frame(&dir, f, "A1").unwrap();
        }
        let series = Series::open(&dir, "A1");
        let labels: Vec<&str> = series.months().iter().map(|m| m.label.as_str()).collect();
        assert!(labels.len() >= 2, "two months, got {:?}", labels);
        let mut sorted = labels.clone();
        sorted.sort();
        assert_eq!(labels, sorted, "oldest month first");
        assert_eq!(series.frames(), frames.len() + 1);
    }

    /// The serial is read off the filename, and a serial with a dash in it
    /// does not lose its last two groups to a month that is not there.
    #[test]
    fn counters_are_found_by_name_and_a_dash_is_not_a_month() {
        let dir = tempdir("names");
        for name in ["random-A-B-2026-01.bin", "random-A-B.bin",
                     "random-F488.bin", "random-.bin", "notmine.bin"] {
            fs::write(dir.join(name), b"").unwrap();
        }
        assert_eq!(counters(&dir), vec!["A-B".to_string(), "F488".to_string()]);
    }

    /// The spectrum recomputed from the samples agrees with the flag the
    /// recorder wrote, on data that was drawn with the flag clear.
    #[test]
    fn the_spectrum_recomputes_the_flag() {
        let frames = entropy::read_frames(&fixture("frames.bin"));
        for f in &frames {
            let counts = f.counts();
            if let Some(s) = spectrum(&counts) {
                assert!(s.runs > 0);
                assert!(s.relative.iter().all(|v| v.is_finite()));
                assert!(s.chance_max > 1.0);
            }
        }
    }

    /// Neither form is guessed at: bytes are bytes and text is JSON.
    #[test]
    fn read_any_takes_either_form_and_refuses_a_third() {
        let dir = tempdir("either");
        let frames = entropy::read_frames(&fixture("frames.bin"));
        let json = dir.join("f.json");
        fs::write(&json, to_json(&frames)).unwrap();
        assert_eq!(read_any(&json).unwrap(), frames);
        assert_eq!(read_any(&fixture("frames.bin")).unwrap(), frames);
        let junk = dir.join("junk.txt");
        fs::write(&junk, b"not a frame").unwrap();
        assert!(read_any(&junk).is_err());
    }
}
