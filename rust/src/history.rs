// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// The counter's own recorded history, decoded.
//
// THE FORMAT IS BEST-EFFORT AND THE RAW BYTES ARE ALWAYS KEPT. GQ's history
// encoding is a byte stream with 0x55 0xAA escape sequences, it has changed
// between firmware revisions, and the only authority is a PDF that disagrees
// with some shipped units. So a download writes the flash image to disk first
// and decodes second: a decoder that mis-reads a marker costs a bad CSV,
// never the data, and a better decoder can be run over the same image later.
//
//   55 AA 00 YY MM DD HH MM SS          a timestamp -- NINE bytes, no mode
//   55 AA 01                            follows every timestamp; three bytes
//   55 AA 02 <len> <ascii...>           a note typed on the device
//   FF                                  unwritten flash
//   anything else                       one count, one byte
//
// THE DATETIME RECORD IS NINE BYTES. GQ's document describes ten, the last a
// save-mode byte. It is not there on GMC-320Re 4.26: over a 16 KiB image all
// 85 datetime records were followed immediately by 0x55, the first byte of
// the next marker. Reading a tenth byte swallowed that 0x55, left 0xAA to be
// decoded as an ordinary sample, and so INVENTED a count of 170 every three
// minutes.
//
// AND 55 AA 01 CARRIES NO PAYLOAD. GQ's document calls it a two-byte count
// for a second whose count did not fit in a byte, and reading it that way
// produced 1,701 readings between 256 and 21,930 counts per second on a tube
// that saturates three orders of magnitude below that. All 4,709 of them sit
// exactly nine bytes after a timestamp, their two "payload" bytes have the
// same distribution as ordinary counts, and read as a marker a three-minute
// stretch holds 180 samples over 181 seconds rather than 179.
//
// Both corrections are the Python's, and tests/test_differential.py decodes
// the same images with both programs and compares every record.
use crate::clock;

#[derive(Debug, PartialEq)]
pub enum Raw {
    Mark { off: usize, when: f64 },
    Count { off: usize, value: u8 },
    Note { off: usize, text: String },
}

/// Every record in an image, one pass, nothing kept.
///
/// A 1 MiB image holds about a million samples and this runs on machines with
/// 512 MB, so no stage of reading history is allowed to build a list with one
/// entry per sample. An iterator rather than a Vec, for exactly that reason.
pub struct RawIter<'a> {
    blob: &'a [u8],
    i: usize,
}

pub fn raw(blob: &[u8]) -> RawIter<'_> {
    RawIter { blob, i: 0 }
}

impl Iterator for RawIter<'_> {
    type Item = Raw;

    fn next(&mut self) -> Option<Raw> {
        let n = self.blob.len();
        while self.i < n {
            let i = self.i;
            let b = self.blob[i];
            if b == 0x55 && i + 3 <= n && self.blob[i + 1] == 0xAA {
                let kind = self.blob[i + 2];
                if kind == 0x00 && i + 9 <= n {
                    let (yy, mm, dd) = (
                        self.blob[i + 3] as i32,
                        self.blob[i + 4] as i32,
                        self.blob[i + 5] as i32,
                    );
                    let (hh, mi, ss) = (
                        self.blob[i + 6] as i32,
                        self.blob[i + 7] as i32,
                        self.blob[i + 8] as i32,
                    );
                    // Range-checked BEFORE mktime, which normalises rather
                    // than refuses.
                    let when = if (1..=12).contains(&mm)
                        && (1..=31).contains(&dd)
                        && hh <= 23
                        && mi <= 59
                        && ss <= 60
                    {
                        clock::from_parts(2000 + yy, mm, dd, hh, mi, ss)
                    } else {
                        None
                    };
                    self.i = i + 9;
                    if let Some(w) = when {
                        return Some(Raw::Mark { off: i, when: w });
                    }
                    continue;
                }
                if kind == 0x01 && i + 3 <= n {
                    self.i = i + 3;
                    continue;
                }
                if kind == 0x02 && i + 4 <= n {
                    let ln = self.blob[i + 3] as usize;
                    let end = (i + 4 + ln).min(n);
                    let text = String::from_utf8_lossy(&self.blob[i + 4..end])
                        .into_owned();
                    self.i = i + 4 + ln;
                    return Some(Raw::Note { off: i, text });
                }
            }
            if b == 0xFF {
                self.i = i + 1;
                continue;
            }
            self.i = i + 1;
            return Some(Raw::Count { off: i, value: b });
        }
        None
    }
}

/// [(offset, unix time, samples before the next mark)] -- one per timestamp.
///
/// Small: a timestamp every three minutes is about 5,400 of these in a full
/// 1 MiB image, against a million samples. Everything else about the
/// recording is derived from this list.
pub fn marks(blob: &[u8]) -> Vec<(usize, f64, usize)> {
    let mut out: Vec<(usize, f64, usize)> = Vec::new();
    let mut since = 0usize;
    for r in raw(blob) {
        match r {
            Raw::Mark { off, when } => {
                if let Some(last) = out.last_mut() {
                    last.2 = since;
                }
                out.push((off, when, 0));
                since = 0;
            }
            Raw::Count { .. } => since += 1,
            Raw::Note { .. } => {}
        }
    }
    if let Some(last) = out.last_mut() {
        last.2 = since;
    }
    out
}

/// Seconds per sample for each stretch between two timestamps.
///
/// THE INTERVAL IS MEASURED, NOT ASSUMED. The counter saves one sample per
/// "second" and its second is not one of ours: on the unit this was written
/// against, 179 samples land between marks 181 seconds apart -- 1.011 s each,
/// which is 39 seconds of drift an hour.
///
/// BUT A MEASUREMENT IS NOT ALWAYS OF WHAT IT LOOKS LIKE. A stretch whose
/// mark is followed four hours later did not record one sample every eighty
/// seconds -- the counter was switched off, and its samples belong in the
/// three minutes after their own mark with a four-hour hole after them. So
/// the median is the recording's real rate, and any stretch more than a
/// factor of two either side of it is given the median instead. The hole then
/// survives as a hole, which is what it was.
pub fn sample_intervals(marks: &[(usize, f64, usize)]) -> Vec<Option<f64>> {
    let mut raw_dts: Vec<Option<f64>> = vec![None; marks.len()];
    for j in 0..marks.len().saturating_sub(1) {
        let span = marks[j + 1].1 - marks[j].1;
        let n = marks[j].2;
        if n > 0 && span > 0.0 {
            raw_dts[j] = Some(span / n as f64);
        }
    }
    let mut good: Vec<f64> = raw_dts.iter().filter_map(|x| *x).collect();
    if good.is_empty() {
        return raw_dts;
    }
    good.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = good[good.len() / 2];
    raw_dts
        .into_iter()
        .map(|x| match x {
            Some(v) if 0.5 * median <= v && v <= 2.0 * median => Some(v),
            _ => Some(median),
        })
        .collect()
}

/// (offset, when, dt, count, note) for every record in an image.
///
/// `when` is a unix time on THE COUNTER'S OWN CLOCK -- correcting that to
/// this machine's is a separate step, because it is a separate error with a
/// separate cause. Records before the first timestamp carry no time: a
/// partial read of the middle of the flash cannot honestly place them.
pub struct Decoded {
    pub off: usize,
    pub when: Option<f64>,
    pub dt: Option<f64>,
    pub count: Option<u8>,
    pub note: String,
}

pub fn records(blob: &[u8]) -> Vec<Decoded> {
    let m = marks(blob);
    let dts = sample_intervals(&m);
    let at: std::collections::HashMap<usize, usize> =
        m.iter().enumerate().map(|(j, x)| (x.0, j)).collect();
    let mut out = Vec::new();
    let mut base: Option<f64> = None;
    let mut dt: Option<f64> = None;
    let mut k = 0usize;
    for r in raw(blob) {
        match r {
            Raw::Mark { off, when } => {
                let j = at.get(&off).copied();
                base = Some(when);
                dt = j.and_then(|j| dts[j]);
                k = 0;
                out.push(Decoded { off, when: Some(when), dt, count: None,
                                   note: String::new() });
            }
            Raw::Count { off, value } => match (base, dt) {
                (Some(b), Some(d)) => {
                    out.push(Decoded { off, when: Some(b + k as f64 * d),
                                       dt: Some(d), count: Some(value),
                                       note: String::new() });
                    k += 1;
                }
                _ => out.push(Decoded { off, when: None, dt: None,
                                        count: Some(value),
                                        note: String::new() }),
            },
            Raw::Note { off, text } => {
                out.push(Decoded { off, when: base, dt, count: None, note: text })
            }
        }
    }
    out
}

/// (unix time, count, seconds this sample covers), with `offset` added to
/// every timestamp to land on this machine's clock.
pub fn samples(blob: &[u8], offset: f64) -> Vec<(f64, u8, Option<f64>)> {
    records(blob)
        .into_iter()
        .filter_map(|r| match (r.when, r.count) {
            (Some(w), Some(c)) => Some((w + offset, c, r.dt)),
            _ => None,
        })
        .collect()
}

/// Replay timed samples into log rows, one per slot, gaps ending averages.
///
/// A hole in the recording is not a quiet stretch: the counter was off, or
/// out of reach. Averages must not be carried across one.
pub fn rows_from_samples(
    samples: &[(f64, u8, Option<f64>)],
    spans: &[f64],
    every: f64,
    max_gap: f64,
    src: &str,
    site_for: &dyn Fn(f64) -> String,
) -> Vec<(f64, String)> {
    use crate::analysis::Windows;
    use crate::log;
    let mut rows: Vec<(f64, String)> = Vec::new();
    let mut w = Windows::new(spans);
    let mut iv = log::Interval::new(spans.len());
    let mut slot: Option<i64> = None;
    let mut last: Option<f64> = None;

    macro_rules! flush {
        ($w:expr, $iv:expr, $at:expr) => {
            if $iv.seconds > 0.0 {
                let averages: Vec<Option<f64>> =
                    spans.iter().map(|s| $w.average(*s)).collect();
                let at: f64 = $at;
                rows.push((
                    at,
                    log::row(at, $iv.cps(), $iv.counts, $iv.seconds, &averages,
                             &$iv.peaks, src, &site_for(at)),
                ));
            }
            $iv.reset();
        };
    }

    for (when, count, dt) in samples {
        if let Some(l) = last {
            if when - l > max_gap {
                flush!(w, iv, slot.unwrap_or(0) as f64 * every);
                w = Windows::new(spans);
                slot = None;
            }
        }
        let here = log::slot_of(*when, every);
        match slot {
            None => slot = Some(here),
            Some(s) if here != s => {
                flush!(w, iv, s as f64 * every);
                slot = Some(here);
            }
            _ => {}
        }
        w.add(*when, *count as u32);
        let averages: Vec<Option<f64>> =
            spans.iter().map(|s| w.average(*s)).collect();
        iv.add(*count as u32, &averages, dt.unwrap_or(1.0));
        last = Some(*when);
    }
    if let Some(s) = slot {
        flush!(w, iv, s as f64 * every);
    }
    rows
}

/// What a backfill did, for saying out loud.
pub struct Report {
    pub samples: usize,
    pub rows: usize,
    pub added: usize,
    pub clashed: usize,
    pub first: Option<f64>,
    pub last: Option<f64>,
    pub holes: usize,
    pub files: Vec<std::path::PathBuf>,
}

/// Fold whatever the image can account for into the log.
pub fn backfill(
    blob: &[u8],
    spans: &[f64],
    every: f64,
    max_gap: f64,
    offset: f64,
    dir: &std::path::Path,
    serial: Option<&str>,
    sites: &[(String, f64, String)],
    one_file: Option<&std::path::Path>,
) -> Report {
    use crate::log;
    // SORTED, because an image is not always in time order. A full dump of a
    // wrapped ring has one step backwards in it, and replaying that as it
    // lies would end every average at the seam and file half the week under
    // the wrong slots.
    let mut samples = samples(blob, offset);
    samples.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
    if samples.is_empty() {
        return Report { samples: 0, rows: 0, added: 0, clashed: 0,
                        first: None, last: None, holes: 0, files: Vec::new() };
    }
    let serial_owned = serial.unwrap_or("").to_string();
    let site_for = |when: f64| -> String {
        if serial_owned.is_empty() {
            String::new()
        } else {
            log::site_at(&serial_owned, when, sites).unwrap_or_default()
        }
    };
    let rows = rows_from_samples(&samples, spans, every, max_gap,
                                 log::SRC_FLASH, &site_for);
    let head = log::header(spans);
    let mut files: std::collections::BTreeMap<std::path::PathBuf,
                                              Vec<(f64, String)>> =
        std::collections::BTreeMap::new();
    match one_file {
        Some(p) => {
            files.insert(p.to_path_buf(), rows.clone());
        }
        None => {
            for r in &rows {
                files.entry(log::path(r.0, dir, serial))
                    .or_default()
                    .push(r.clone());
            }
        }
    }
    let (mut added, mut clashed) = (0usize, 0usize);
    for (path, group) in &files {
        if let Ok((a, c)) = log::merge(path, &head, group, every) {
            added += a;
            clashed += c;
        }
    }
    // Holes are worth counting and saying out loud. They are the hours the
    // counter was off or out of reach, they are not filled, and a person
    // looking at a suspiciously short file deserves to know there were 42 of
    // them rather than wondering what the backfill lost.
    let holes = rows.windows(2).filter(|p| p[1].0 - p[0].0 > every).count();
    Report {
        samples: samples.len(),
        rows: rows.len(),
        added,
        clashed,
        first: Some(samples[0].0),
        last: Some(samples[samples.len() - 1].0),
        holes,
        files: files.keys().cloned().collect(),
    }
}

// ----------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    /// A history image built out of a little script, so a test can say what
    /// it is testing rather than a wall of hex.
    fn mark(y: u8, mo: u8, d: u8, h: u8, mi: u8, s: u8) -> Vec<u8> {
        vec![0x55, 0xAA, 0x00, y, mo, d, h, mi, s]
    }
    fn tick() -> Vec<u8> {
        vec![0x55, 0xAA, 0x01]
    }
    fn note(text: &str) -> Vec<u8> {
        let mut v = vec![0x55, 0xAA, 0x02, text.len() as u8];
        v.extend_from_slice(text.as_bytes());
        v
    }
    fn image(parts: &[Vec<u8>]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn a_timestamp_is_nine_bytes_and_the_marker_after_it_is_three() {
        // Read the datetime as ten bytes and it swallows the 0x55 of the next
        // marker, leaving 0xAA to decode as a count of 170. Read 55 AA 01 as
        // a two-byte count and it invents readings in the tens of thousands
        // per second. Both are what GQ's document says, and both are wrong.
        let img = image(&[
            mark(26, 9, 4, 12, 0, 0), tick(), vec![1, 2],
            mark(26, 9, 4, 12, 0, 3), tick(), vec![3],
        ]);
        let counts: Vec<u8> = raw(&img)
            .filter_map(|r| match r {
                Raw::Count { value, .. } => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(counts, vec![1, 2, 3], "no 170, no swallowed marker");
        assert_eq!(marks(&img).len(), 2);
    }

    #[test]
    fn unwritten_flash_is_absence_and_not_a_count_of_255() {
        let img = image(&[
            mark(26, 9, 4, 12, 0, 0), tick(), vec![1],
            vec![0xFF; 16],
            mark(26, 9, 4, 12, 0, 2), tick(), vec![2],
        ]);
        let counts: Vec<u8> = raw(&img)
            .filter_map(|r| match r {
                Raw::Count { value, .. } => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(counts, vec![1, 2]);
    }

    #[test]
    fn a_corrupt_timestamp_is_refused_rather_than_normalised() {
        // mktime turns month 99 day 99 into 2034 rather than refusing, and a
        // mark accepted from half-erased flash would place every sample after
        // it in the wrong decade.
        for bad in [(99u8, 99u8, 0u8, 0u8, 0u8), (0, 1, 0, 0, 0),
                    (1, 0, 0, 0, 0), (1, 1, 99, 0, 0), (1, 1, 0, 99, 0),
                    (1, 1, 0, 0, 99)] {
            let img = image(&[
                mark(26, bad.0, bad.1, bad.2, bad.3, bad.4),
                tick(),
                vec![1, 2],
            ]);
            assert!(marks(&img).is_empty(), "accepted {:?}", bad);
        }
    }

    #[test]
    fn a_note_is_text_and_does_not_count_as_a_sample() {
        let img = image(&[
            mark(26, 9, 4, 12, 0, 0), tick(), vec![1],
            note("bench"), vec![2],
        ]);
        let notes: Vec<String> = raw(&img)
            .filter_map(|r| match r {
                Raw::Note { text, .. } => Some(text),
                _ => None,
            })
            .collect();
        assert_eq!(notes, vec!["bench".to_string()]);
        assert_eq!(marks(&img)[0].2, 2, "the note is not one of the two counts");
    }

    #[test]
    fn a_record_cut_off_by_the_end_of_a_tail_read_does_not_panic() {
        let base = image(&[mark(26, 9, 4, 12, 0, 0), tick(), vec![1, 2, 3]]);
        for tail in [vec![0x55], vec![0x55, 0xAA], vec![0x55, 0xAA, 0x00],
                     vec![0x55, 0xAA, 0x00, 26, 9], vec![0x55, 0xAA, 0x02],
                     vec![0x55, 0xAA, 0x02, 40, 0x61]] {
            let mut img = base.clone();
            img.extend_from_slice(&tail);
            let _ = records(&img); // must not panic, and must terminate
        }
    }

    #[test]
    fn the_interval_is_measured_rather_than_assumed() {
        // Nine samples between marks ten seconds apart is 1.111 s each. The
        // counter's second is not ours, and assuming 1.000 files an hour-old
        // sample most of a minute from where it belongs.
        let img = image(&[
            mark(26, 9, 4, 12, 0, 0), tick(), vec![1; 9],
            mark(26, 9, 4, 12, 0, 10), tick(), vec![1; 9],
            mark(26, 9, 4, 12, 0, 20), tick(), vec![1; 3],
        ]);
        let dts = sample_intervals(&marks(&img));
        assert!((dts[0].unwrap() - 10.0 / 9.0).abs() < 1e-9, "{:?}", dts[0]);
    }

    #[test]
    fn a_hole_is_a_hole_and_not_a_very_slow_stretch() {
        // A mark followed four hours later did not record one sample every
        // eighty seconds: the counter was off. The median stands in, and the
        // hole survives as a hole.
        let img = image(&[
            mark(26, 9, 4, 12, 0, 0), tick(), vec![1; 9],
            mark(26, 9, 4, 12, 0, 10), tick(), vec![1; 9],
            mark(26, 9, 4, 12, 0, 20), tick(), vec![1; 9],
            mark(26, 9, 4, 16, 0, 0), tick(), vec![1; 9],
            mark(26, 9, 4, 16, 0, 10), tick(), vec![1; 3],
        ]);
        let dts = sample_intervals(&marks(&img));
        let median = 10.0 / 9.0;
        assert!((dts[2].unwrap() - median).abs() < 1e-9,
                "the four-hour stretch took the median, got {:?}", dts[2]);
    }

    #[test]
    fn one_timestamp_measures_nothing_and_says_so() {
        let img = image(&[mark(26, 9, 4, 12, 0, 0), tick(), vec![1, 2, 3]]);
        assert_eq!(sample_intervals(&marks(&img)), vec![None]);
        // And so nothing can be placed: a time without a spacing is not a
        // position, and guessing one second would be quietly wrong.
        assert!(samples(&img, 0.0).is_empty());
    }

    #[test]
    fn samples_before_the_first_timestamp_are_not_placed() {
        let img = image(&[
            vec![5, 6, 7],
            mark(26, 9, 4, 12, 0, 0), tick(), vec![1, 2],
            mark(26, 9, 4, 12, 0, 3), tick(), vec![0],
        ]);
        let placed = samples(&img, 0.0);
        assert!(placed.iter().all(|s| s.1 < 5),
                "a partial read of the middle of the flash was placed anyway");
    }

    #[test]
    fn the_offset_moves_every_sample_and_nothing_else() {
        let img = image(&[
            mark(26, 9, 4, 12, 0, 0), tick(), vec![1, 2],
            mark(26, 9, 4, 12, 0, 3), tick(), vec![3],
        ]);
        let a = samples(&img, 0.0);
        let b = samples(&img, 1800.0);
        assert_eq!(a.len(), b.len());
        for (x, y) in a.iter().zip(b.iter()) {
            assert!((y.0 - x.0 - 1800.0).abs() < 1e-9);
            assert_eq!(x.1, y.1);
        }
    }

    #[test]
    fn an_empty_or_erased_image_decodes_to_nothing() {
        assert!(records(&[]).is_empty());
        assert!(records(&[0xFF; 64]).is_empty());
        assert!(marks(&[0xFF; 64]).is_empty());
        assert!(sample_intervals(&[]).is_empty());
    }

    #[test]
    fn a_marker_byte_on_its_own_is_an_ordinary_count() {
        // 0x55 not followed by 0xAA is a count of 85; 0xAA alone is 170.
        let img = image(&[
            mark(26, 9, 4, 12, 0, 0), tick(),
            vec![0x55, 0x01, 0xAA],
            mark(26, 9, 4, 12, 0, 3), tick(), vec![1],
        ]);
        let counts: Vec<u8> = raw(&img)
            .filter_map(|r| match r {
                Raw::Count { value, .. } => Some(value),
                _ => None,
            })
            .collect();
        assert_eq!(counts, vec![0x55, 0x01, 0xAA, 1]);
    }
}
