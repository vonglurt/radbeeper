// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// What the audit page carries: one row per emission, joined to the count log
// that was running at the time, and the raw frames behind the recent ones.
//
// THE DIVISION OF LABOUR THAT MAKES THIS POSSIBLE AT ALL. `random.html` is
// compared byte for byte against the page ./radbeeper writes, and ./radbeeper
// has never heard of a frame. So nothing here parses one. The joined table is
// built from the emission .tsv and the count .tsv -- two text formats both
// implementations have always read -- and the frames are copied into the page
// as base64 without being looked at. The decoding happens in src/frames.js,
// in the browser, where there is exactly one implementation of it.
//
// The chain IS computed here, in both languages, because it is computed from
// the keys in the emission log and not from the frames. That is deliberate:
// it keeps the tamper-evidence inside the differential contract, so the
// reference implementation can contradict the Rust about it.
use crate::entropy;
use crate::log;
use crate::sha256::{hex, Sha256};
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

/// How many raw bytes of frame data a page embeds before it starts leaving
/// months out. Two mebibytes is about three weeks of continuous counting.
///
/// A BUDGET RATHER THAN A CUTOFF IN TIME, because the thing that hurts is
/// page weight and page weight is bytes. A quiet counter fits a year in here;
/// a busy one fits a fortnight; both produce a page that loads.
pub const DEFAULT_FRAME_BUDGET: u64 = 2 * 1024 * 1024;

const B64: &[u8; 64] =
    b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/// Base64 with a newline every 76 characters and one at the end.
///
/// WRAPPED THE WAY python's base64.encodebytes WRAPS, to the character,
/// because the other implementation of this page calls exactly that and the
/// two outputs are compared byte for byte. It also keeps the page diffable:
/// a two-megabyte single line is a file no editor and no review tool will
/// show you.
pub fn base64(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len() * 4 / 3 + data.len() / 57 + 8);
    let mut col = 0;
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(B64[(n >> 18) as usize & 63] as char);
        out.push(B64[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { B64[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { B64[n as usize & 63] as char } else { '=' });
        col += 4;
        if col >= 76 {
            out.push('\n');
            col = 0;
        }
    }
    if col > 0 {
        out.push('\n');
    }
    out
}

/// One emission, with everything known about the moment it was drawn.
pub struct Join {
    pub seq: u64,
    pub started: String,
    pub samples: usize,
    pub counts: u64,
    pub cpm: f64,
    pub usvh: f64,
    pub bits: f64,
    pub flat: bool,
    pub band: &'static str,
    /// The 30-second average the count log was reporting at that moment, or
    /// None where the log has no row near enough to say.
    pub log_cpm: Option<f64>,
    pub key: String,
    pub link: String,
}

/// How far from an emission a count-log row may be and still describe it.
///
/// The service writes a row every thirty seconds by default, so a minute
/// either side always finds one when the log was running, and never invents a
/// reading for an emission the log was not there for.
const JOIN_WINDOW: f64 = 60.0;

/// (time, cpm_30) for one counter, oldest first.
fn log_series(logs: &Path, serial: &str) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::new();
    for (s, path) in log::files(logs) {
        if s != serial {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else { continue };
        let mut cpm30: Option<usize> = None;
        for line in text.lines() {
            if line.starts_with('#') {
                // A file can carry more than one header, where the windows
                // changed mid-month; the column is found by NAME under the
                // last header seen, never by position.
                cpm30 = line[1..].split('\t').position(|h| h == "cpm_30");
                continue;
            }
            if line.trim().is_empty() {
                continue;
            }
            let cells: Vec<&str> = line.split('\t').collect();
            let (Some(i), Some(when)) = (cpm30, cells.first().and_then(|c| crate::clock::parse_stamp(c)))
            else { continue };
            if let Some(v) = cells.get(i).and_then(|c| c.parse::<f64>().ok()) {
                out.push((when, v));
            }
        }
    }
    out.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    out
}

fn nearest(series: &[(f64, f64)], when: f64) -> Option<f64> {
    if series.is_empty() {
        return None;
    }
    let i = series.partition_point(|(t, _)| *t < when);
    let mut best: Option<(f64, f64)> = None;
    for j in [i.saturating_sub(1), i, i + 1] {
        if let Some((t, v)) = series.get(j) {
            let d = (t - when).abs();
            if d <= JOIN_WINDOW && best.map(|(bd, _)| d < bd).unwrap_or(true) {
                best = Some((d, *v));
            }
        }
    }
    best.map(|(_, v)| v)
}

/// Every emission for one counter, chained and joined.
pub fn join(
    logs: &Path,
    serial: &str,
    pools: &[entropy::Emission],
    cpm_per_usvh: f64,
) -> Vec<Join> {
    let series = log_series(logs, serial);
    let mut out = Vec::with_capacity(pools.len());
    let mut prev = entropy::GENESIS_LINK.to_string();
    for p in pools {
        let counts: u64 = p.counts.iter().map(|c| *c as u64).sum();
        let cpm = p.rate * 60.0;
        let when = crate::clock::parse_stamp(&p.time);
        prev = entropy::chain(&prev, &p.hex);
        out.push(Join {
            seq: p.seq,
            started: p.time.clone(),
            samples: p.seconds,
            counts,
            cpm,
            usvh: if cpm_per_usvh > 0.0 { cpm / cpm_per_usvh } else { 0.0 },
            bits: p.bits,
            flat: p.flat,
            band: crate::analysis::band(cpm).name(),
            log_cpm: when.and_then(|w| nearest(&series, w)),
            key: p.hex.clone(),
            link: prev.clone(),
        });
    }
    out
}

/// The joined table as the file the page links and the viewer reads.
pub fn joined_tsv(rows: &[Join], head: &[String]) -> String {
    let mut s = String::new();
    for line in head {
        s.push_str("# ");
        s.push_str(line);
        s.push('\n');
    }
    s.push_str(
        "#seq\tstarted\tsamples\tcounts\tcpm\tusvh\tbits\tflat\t\
         band\tlog_cpm\tkey\tlink\n",
    );
    for r in rows {
        s.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            r.seq,
            r.started,
            r.samples,
            r.counts,
            log::exact(round_to(r.cpm, 4)),
            log::exact(round_to(r.usvh, 6)),
            log::exact(round_to(r.bits, 4)),
            if r.flat { "yes" } else { "no" },
            r.band,
            r.log_cpm.map(|v| log::exact(round_to(v, 1))).unwrap_or_else(|| "--".into()),
            r.key,
            r.link
        ));
    }
    s
}

/// Half-up to `places`, which is what both implementations' formatting does.
fn round_to(v: f64, places: i32) -> f64 {
    let m = 10f64.powi(places);
    (v * m).round() / m
}

/// The frame files to embed, newest first, stopping before the budget.
///
/// WHOLE FILES AND NOT A BYTE COUNT INTO ONE, because the other
/// implementation cannot see where a frame ends -- it is copying bytes it
/// does not parse. A month is a unit both can agree on without either of them
/// understanding the contents.
pub fn frames_to_embed(logs: &Path, serial: &str, budget: u64) -> Vec<PathBuf> {
    let all = entropy::random_series(logs, serial, "bin");
    let mut taken: Vec<PathBuf> = Vec::new();
    let mut total = 0u64;
    for path in all.iter().rev() {
        let size = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        if size == 0 {
            continue;
        }
        // The newest file goes in even when it is over budget on its own: a
        // page with no frames at all is worse than a heavy one, and the
        // alternative is a viewer that silently has nothing to show.
        if !taken.is_empty() && total + size > budget {
            break;
        }
        total += size;
        taken.push(path.clone());
    }
    taken.reverse();
    taken
}

/// Those files' bytes, concatenated in order.
pub fn frames_bytes(paths: &[PathBuf]) -> Vec<u8> {
    let mut out = Vec::new();
    for p in paths {
        if let Ok(b) = fs::read(p) {
            out.extend_from_slice(&b);
        }
    }
    out
}

/// Every emission file for a counter, as (name, bytes), for the page to link.
pub fn downloads(logs: &Path, serial: &str) -> Vec<(String, u64)> {
    let mut out: BTreeMap<String, u64> = BTreeMap::new();
    for ext in ["tsv", "hex", "bin"] {
        for p in entropy::random_series(logs, serial, ext) {
            let size = fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            if size > 0 {
                out.insert(
                    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(),
                    size,
                );
            }
        }
    }
    out.into_iter().collect()
}

/// A digest over the joined table, so the page can name what it is showing.
pub fn table_digest(tsv: &str) -> String {
    let mut h = Sha256::new();
    h.update(tsv.as_bytes());
    hex(&h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_wraps_where_python_wraps() {
        // 57 bytes is exactly one 76-character line, which is the boundary
        // the wrapping is defined by.
        let data: Vec<u8> = (0..57u8).collect();
        let s = base64(&data);
        let lines: Vec<&str> = s.trim_end().split('\n').collect();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].len(), 76);
        assert!(s.ends_with('\n'));

        let two = base64(&(0..58u8).collect::<Vec<u8>>());
        assert_eq!(two.trim_end().split('\n').count(), 2);
    }

    #[test]
    fn base64_pads_the_way_everyone_else_does() {
        assert_eq!(base64(b"").trim_end(), "");
        assert_eq!(base64(b"f").trim_end(), "Zg==");
        assert_eq!(base64(b"fo").trim_end(), "Zm8=");
        assert_eq!(base64(b"foo").trim_end(), "Zm9v");
        assert_eq!(base64(b"foob").trim_end(), "Zm9vYg==");
        assert_eq!(base64(b"fooba").trim_end(), "Zm9vYmE=");
        assert_eq!(base64(b"foobar").trim_end(), "Zm9vYmFy");
    }

    #[test]
    fn the_newest_month_is_embedded_even_when_it_alone_is_over_budget() {
        let dir = std::env::temp_dir().join(format!("rb-budget-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("random-A-2026-01.bin"), vec![0u8; 100]).unwrap();
        fs::write(dir.join("random-A-2026-02.bin"), vec![0u8; 5000]).unwrap();
        let got = frames_to_embed(&dir, "A", 1000);
        let names: Vec<String> = got
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["random-A-2026-02.bin".to_string()]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_emission_with_no_log_beside_it_joins_to_nothing_rather_than_to_zero() {
        let dir = std::env::temp_dir().join(format!("rb-join-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let pools = vec![entropy::Emission {
            seq: 0,
            time: "2026-09-01T00:00:00".into(),
            seconds: 3,
            rate: 1.0,
            bits: 2.0,
            flat: true,
            hex: "aa".repeat(32),
            counts: vec![1, 1, 1],
        }];
        let rows = join(&dir, "NOPE", &pools, 151.5);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].log_cpm.is_none(), "invented a reading out of no log");
        assert_eq!(rows[0].link, entropy::chain(entropy::GENESIS_LINK, &pools[0].hex));
        let _ = fs::remove_dir_all(&dir);
    }
}
