// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// The log format: read, write, merge, rotate.
//
// BYTE-FOR-BYTE THE PYTHON'S. This is not a reimplementation that produces
// something equivalent, it is one that produces the same characters, and
// tests/test_differential.py checks that against the Python on the same
// input. Two dialects of a file format is the thing the split between these
// two programs was arranged to avoid, and the arrangement is only worth
// anything if somebody checks.
use crate::clock;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const DEFAULT_LOG_EVERY: f64 = 30.0;
pub const SRC_LIVE: &str = "live";

/// The name in the merged log's file name, where a counter's serial goes.
///
/// A RESERVED SLOT, NOT A COUNTER. `cpm-merged-YYYY-MM.tsv` sits beside the
/// per-counter files and is deliberately shaped like one, so `ls` and `sort`
/// and a month's rotation all work on it unchanged -- but nothing may mistake
/// it for a tube, or the report would count the room twice and `together`
/// would ask a merge whether it agrees with the counters it is made of.
/// `files()` therefore skips it and `merged_files()` is the only way to it.
///
/// A GMC serial is fourteen hex characters, so no counter can ever be called
/// this.
pub const MERGED: &str = "merged";
#[allow(dead_code)]
pub const SRC_FLASH: &str = "flash";

/// Python's `%g`, which Rust has no formatter for.
///
/// Six significant digits, `%e` when the exponent is below -4 or at least 6,
/// `%f` otherwise, and in both cases trailing zeroes and a trailing point
/// removed. It formats the `seconds` column and every span in the header, so
/// getting it approximately right would mean a header that does not match the
/// one the Python writes -- and columns are matched by name.
pub fn g(v: f64) -> String {
    if v == 0.0 {
        return "0".to_string();
    }
    if !v.is_finite() {
        return format!("{}", v);
    }
    // THE EXPONENT IS THE ONE AFTER ROUNDING, which is the part that is easy
    // to get wrong and impossible to notice. C picks the style from the
    // exponent the value would have in style E at this precision -- so
    // 999999.5 is 1.00000e+06, exponent 6, and prints as 1e+06. Taking the
    // exponent off the unrounded value gives 5, and prints 1000000. Caught
    // by tests/test_differential.py on its first run.
    let sci = format!("{:.*e}", 5, v);
    let (mantissa, exp) = match sci.split_once('e') {
        Some((m, e)) => (m.to_string(), e.parse::<i32>().unwrap_or(0)),
        None => (sci, 0),
    };
    if exp < -4 || exp >= 6 {
        // Rust writes `1.23457e6`; C and Python write `1.23457e+06`.
        format!(
            "{}e{}{:02}",
            trim(&mantissa),
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        )
    } else {
        let places = (5 - exp).max(0) as usize;
        trim(&format!("{:.*}", places, v))
    }
}

fn trim(s: &str) -> String {
    if !s.contains('.') {
        return s.to_string();
    }
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// The header line, '#' first so it sorts above the rows rather than into
/// the middle of them.
pub fn header(spans: &[f64]) -> String {
    let mut head: Vec<String> = ["time", "cps", "counts", "seconds"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    head.extend(spans.iter().map(|s| format!("cpm_{}", g(*s))));
    head.extend(spans.iter().map(|s| format!("peak_{}", g(*s))));
    head.push("src".to_string());
    head.push("site".to_string());
    format!("#{}", head.join("\t"))
}

/// A number written so that reading it back gives the same number.
///
/// THE HIGHEST-QUALITY FIGURE IS NOT THE LONGEST ONE. The obvious reach for
/// "record it precisely" is `{:.17}`, which pads 0.5 out to
/// 0.50000000000000000 and still cannot promise a round trip for every value;
/// the obvious reach for "record it readably" is `%g`, which is what the
/// per-counter log uses and throws away everything past the sixth significant
/// figure. Rust's own Display for f64 writes the SHORTEST string that parses
/// back to the identical bits -- so this is simultaneously the most precise
/// form there is and, for the ordinary values in a log, the shortest.
///
/// It is not the per-counter log's formatter and must never become it: that
/// file's characters are pinned against the Python, byte for byte, by
/// tests/test_differential.py. This is the merged file's, which has no such
/// promise to keep and exists precisely to keep what the other one rounds.
pub fn exact(v: f64) -> String {
    if !v.is_finite() {
        return String::new();
    }
    // `{}` on an f64 that happens to be whole writes "34" and not "34.0",
    // which reads back as the same number and is the shorter of the two.
    format!("{}", v)
}

/// The merged log's header.
///
/// WHAT THIS FILE IS FOR, AND WHY IT IS NOT THE OTHER ONE. The per-counter
/// log is the format of record: one file per instrument, one decimal place,
/// the same characters the Python writes, and a row in it is a reading ONE
/// tube took. That is a promise worth keeping and it is the wrong file to ask
/// a question about two tubes -- the moment a row blended them, no later
/// analysis could unpick which instrument said what.
///
/// So the merge goes beside it rather than into it, and carries both halves
/// of the same interval:
///
///   RAW -- `counts` is every arrival off every tube, an integer, lossless;
///   `tube_seconds` is how much instrument-time produced them; `per_tube`
///   breaks the count down by serial so the merge can be taken apart again.
///
///   MERGED -- `cps` and `cpm_N` are the rate of the ROOM: the counts over
///   the tube-seconds behind them, which is the mean the tubes agree on and
///   not their sum. Two tubes do not double the dose; they halve the error
///   bar, and `sigma` is where that lands.
///
///   INTERLEAVED -- `tubes` and `interleave`, the mean gap between one tube's
///   sample and the next tube's. It is what says whether the merge bought
///   time resolution or only precision: at 1/n of a second the tubes are
///   taking turns, at zero they are firing together.
///
/// `unix` is beside `time` because `time` is whole seconds and a sample is
/// not: the stamp is what a person reads and the epoch is what survives.
pub fn merged_header(spans: &[f64]) -> String {
    let mut head: Vec<String> = [
        "time", "unix", "tubes", "interleave", "counts", "seconds",
        "tube_seconds", "cps", "cps_raw",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    head.extend(spans.iter().map(|s| format!("cpm_{}", g(*s))));
    head.push("sigma_30".to_string());
    head.extend(spans.iter().map(|s| format!("peak_{}", g(*s))));
    head.push("per_tube".to_string());
    head.push("src".to_string());
    head.push("site".to_string());
    format!("#{}", head.join("\t"))
}

/// One row of the merged log. See `merged_header` for what each half is.
#[allow(clippy::too_many_arguments)]
pub fn merged_row(
    when: f64,
    tubes: usize,
    interleave: Option<f64>,
    counts: u64,
    seconds: f64,
    tube_seconds: f64,
    averages: &[Option<f64>],
    sigma: Option<f64>,
    peaks: &[Option<f64>],
    per_tube: &[(String, u64)],
    src: &str,
    site: &str,
) -> String {
    // THE ROOM'S RATE IS COUNTS OVER TUBE-SECONDS. Dividing by the wall clock
    // instead would report two tubes as twice the background, which is the
    // one arithmetic mistake a second instrument makes easy and no reading of
    // "merged" excuses.
    let cps = if tube_seconds > 0.0 { counts as f64 / tube_seconds } else { 0.0 };
    // And the arrival rate at the machine, which is n times it and is what
    // the entropy pool and the cascade's fine tier are actually fed.
    let cps_raw = if seconds > 0.0 { counts as f64 / seconds } else { 0.0 };
    let mut cells = vec![
        clock::stamp(when),
        exact(when),
        format!("{}", tubes),
        interleave.map(exact).unwrap_or_default(),
        format!("{}", counts),
        exact(seconds),
        exact(tube_seconds),
        exact(cps),
        exact(cps_raw),
    ];
    let one = |v: &Option<f64>| v.map(exact).unwrap_or_default();
    cells.extend(averages.iter().map(one));
    cells.push(one(&sigma));
    cells.extend(peaks.iter().map(one));
    cells.push(
        per_tube
            .iter()
            .map(|(serial, n)| format!("{}={}", serial, n))
            .collect::<Vec<_>>()
            .join(","),
    );
    cells.push(src.to_string());
    cells.push(site.to_string());
    cells.join("\t")
}

/// The column names a header line declares, '#' stripped from the first.
pub fn columns(header_line: &str) -> Vec<String> {
    if !header_line.starts_with('#') {
        return Vec::new();
    }
    header_line[1..]
        .trim_end_matches('\n')
        .split('\t')
        .map(|s| s.to_string())
        .collect()
}

/// One row. The timestamp is first and big-endian, so `sort` on the file is
/// chronological with no flags, no field numbers and no awk.
#[allow(clippy::too_many_arguments)]
pub fn row(
    when: f64,
    cps: f64,
    counts: u64,
    seconds: f64,
    averages: &[Option<f64>],
    peaks: &[Option<f64>],
    src: &str,
    site: &str,
) -> String {
    let mut cells = vec![
        clock::stamp(when),
        format!("{:.3}", cps),
        format!("{}", counts),
        g(seconds),
    ];
    let one = |v: &Option<f64>| match v {
        Some(x) => format!("{:.1}", x),
        None => String::new(),
    };
    cells.extend(averages.iter().map(one));
    cells.extend(peaks.iter().map(one));
    cells.push(src.to_string());
    cells.push(site.to_string());
    cells.join("\t")
}

/// The dated log a row belongs in: one file per counter per month.
///
/// Rotation by construction, which is why there is no rotation code. A row is
/// written to the file for its own month, so a month ending is not an event:
/// that file stops growing and the next one starts. Nothing is scheduled and
/// nothing renames a file while a service is appending to it.
pub fn path(when: f64, directory: &Path, serial: Option<&str>) -> PathBuf {
    directory.join(format!(
        "cpm-{}-{}.tsv",
        serial.unwrap_or("unknown"),
        clock::format(when, "%Y-%m")
    ))
}

/// [(serial, path)] for every dated log in a directory, oldest name first.
pub fn files(directory: &Path) -> Vec<(String, PathBuf)> {
    let mut names: Vec<String> = match fs::read_dir(directory) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(_) => return Vec::new(),
    };
    names.sort();
    let mut out = Vec::new();
    for name in names {
        if !name.starts_with("cpm-") || !name.ends_with(".tsv") {
            continue;
        }
        let stem = &name[4..name.len() - 4];
        // cpm-<serial>-YYYY-MM.tsv, and a serial may contain dashes we do not
        // care about: split from the right, where the date is.
        let mut parts = stem.rsplitn(3, '-');
        if parts.next().is_none() || parts.next().is_none() {
            continue;
        }
        let serial = match parts.next() {
            Some(s) if !s.is_empty() => s.to_string(),
            _ => continue,
        };
        // THE MERGE IS NOT A TUBE. It is shaped like one on disk so that
        // rotation and sorting need no special case, and a report that let it
        // through here would count the room once per counter and once again
        // for the merge of them -- and then ask the merge whether it agreed
        // with the counters it was made of. See MERGED.
        if serial == MERGED {
            continue;
        }
        out.push((serial, directory.join(&name)));
    }
    out
}

/// The merged logs in a directory, oldest name first.
///
/// The other side of the filter in `files`: everything it skips, and nothing
/// it returns.
pub fn merged_files(directory: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = match fs::read_dir(directory) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| {
                n.starts_with(&format!("cpm-{}-", MERGED)) && n.ends_with(".tsv")
            })
            .map(|n| directory.join(n))
            .collect(),
        Err(_) => return Vec::new(),
    };
    out.sort();
    out
}

/// The unix time a written row carries, or None if it is not a row.
pub fn row_time(line: &str) -> Option<f64> {
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    clock::parse_stamp(line.split('\t').next()?)
}

/// One row re-columned from the header it was written under.
///
/// COLUMNS ARE MATCHED BY NAME, NOT BY POSITION. Adding a span inserts
/// `cpm_3000` after `cpm_300` and `peak_3000` after `peak_300` -- in the
/// middle of the row, twice. Padding such a row on the right slides every
/// value after the insertion point one column left and writes a counter's
/// `src` into a `peak` column. The row still parses; it is just wrong.
pub fn align_row(
    cells: &[&str],
    from_names: &[String],
    to_names: &[String],
) -> Vec<String> {
    to_names
        .iter()
        .map(|name| match from_names.iter().position(|f| f == name) {
            Some(i) => cells.get(i).copied().unwrap_or("").to_string(),
            None => String::new(),
        })
        .collect()
}

/// (when, cells) for a log written under any header, aligned to `to_names`.
pub fn read_table(path: &Path, to_names: &[String]) -> Vec<(f64, Vec<String>)> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut from_names: Vec<String> = to_names.to_vec();
    let mut saw_header = false;
    let mut out = Vec::new();
    for line in text.lines() {
        if line.starts_with('#') {
            let names = columns(line);
            if !names.is_empty() {
                from_names = names;
                saw_header = true;
            }
            continue;
        }
        let when = match row_time(line) {
            Some(w) => w,
            None => continue,
        };
        let cells: Vec<&str> = line.split('\t').collect();
        let mut aligned = align_row(&cells, &from_names, to_names);
        // A file written before the src column existed predates the backfill
        // that made the column necessary, so every row in it was measured
        // here. That is knowledge, not a guess.
        if saw_header && !from_names.iter().any(|n| n == "src") {
            if let Some(i) = to_names.iter().position(|n| n == "src") {
                aligned[i] = SRC_LIVE.to_string();
            }
        }
        out.push((when, aligned));
    }
    out
}

/// The slot a time falls in. This is a row's identity.
pub fn slot_of(when: f64, every: f64) -> i64 {
    (when / every).floor() as i64
}

/// Fold rows into the log, one per slot, never over one already there.
///
/// Returns (added, clashed). A clash is not an error and not a warning: it is
/// the ordinary case of backfilling a period that was already logged live,
/// and the live row is the better evidence, so it stays.
///
/// The file is rewritten rather than appended to, because backfilled rows
/// belong in the PAST and the one property this format promises is that plain
/// `sort` on it is chronological. Written to a temporary file in the same
/// directory and renamed over the original, so an interrupted backfill leaves
/// the old log intact rather than half a new one.
pub fn merge(
    path: &Path,
    head: &str,
    rows: &[(f64, String)],
    every: f64,
) -> std::io::Result<(usize, usize)> {
    let names = columns(head);
    let existing: Vec<(f64, String)> = read_table(path, &names)
        .into_iter()
        .map(|(w, cells)| (w, cells.join("\t")))
        .collect();
    let mut taken: Vec<i64> =
        existing.iter().map(|(w, _)| slot_of(*w, every)).collect();
    taken.sort_unstable();
    taken.dedup();
    let mut keep = Vec::new();
    let mut clashed = 0;
    for (when, line) in rows {
        let slot = slot_of(*when, every);
        if taken.binary_search(&slot).is_ok() {
            clashed += 1;
            continue;
        }
        let at = taken.partition_point(|s| *s < slot);
        taken.insert(at, slot);
        keep.push((*when, line.clone()));
    }
    let added = keep.len();
    if added == 0 {
        return Ok((0, clashed));
    }
    let mut merged: Vec<(f64, String)> = existing;
    merged.extend(keep);
    // By the LINE, not by the time: the timestamp is the first column and
    // sorts lexicographically, which is the promise the format makes.
    merged.sort_by(|a, b| a.1.cmp(&b.1));
    let tmp = path.with_extension("tsv.new");
    {
        let mut f = fs::File::create(&tmp)?;
        writeln!(f, "{}", head)?;
        for (_w, line) in &merged {
            writeln!(f, "{}", line)?;
        }
        f.flush()?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok((added, clashed))
}

// ----------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    /// THE MERGED LOG'S FIGURES READ BACK AS THE FIGURES THAT WERE WRITTEN.
    /// That is the whole of what `exact` is for and the one property worth
    /// pinning: not how many digits it uses, which is its business, but that
    /// no bit is lost between the two.
    #[test]
    fn an_exact_number_parses_back_to_the_same_bits() {
        let cases = [
            0.0, 0.5, 1.0, 34.0, 1.0 / 3.0, 60.0 / 7.0, 1789832801.956088,
            0.0001234567890123, 1.7976931348623157e308, 5e-324,
            96.92307692307692, -12.5,
        ];
        for v in cases {
            let text = exact(v);
            let back: f64 = text.parse().expect(&text);
            assert_eq!(back.to_bits(), v.to_bits(), "{} -> {}", v, text);
        }
        // And it is the SHORT form, not seventeen padded places: the point is
        // precision, and precision is not the same thing as length.
        assert_eq!(exact(0.5), "0.5");
        assert_eq!(exact(34.0), "34");
        // Nothing that cannot be read back is written at all.
        assert_eq!(exact(f64::NAN), "");
        assert_eq!(exact(f64::INFINITY), "");
    }

    /// THE PER-COUNTER HEADER IS NOT TOUCHED BY ANY OF THIS. It is pinned
    /// against the Python byte for byte by tests/test_differential.py, and
    /// the merged log exists precisely so that it never has to change.
    #[test]
    fn the_merged_header_is_a_second_format_and_not_a_change_to_the_first() {
        let spans = [3.0, 30.0, 300.0, 3000.0, 30000.0];
        assert_eq!(
            header(&spans),
            "#time\tcps\tcounts\tseconds\tcpm_3\tcpm_30\tcpm_300\tcpm_3000\tcpm_30000\t\
             peak_3\tpeak_30\tpeak_300\tpeak_3000\tpeak_30000\tsrc\tsite"
        );
        let m = columns(&merged_header(&spans));
        // Both halves of every interval are named in it.
        for want in ["counts", "tube_seconds", "per_tube", "cps", "cps_raw",
                     "tubes", "interleave", "cpm_30", "sigma_30", "unix"] {
            assert!(m.contains(&want.to_string()), "no {} in {:?}", want, m);
        }
        // And a row fills exactly the columns the header declares.
        let row = merged_row(
            1_700_000_000.5, 2, Some(0.5413), 34, 30.0, 60.0,
            &[Some(70.0), Some(34.0), None, None, None],
            Some(4.2),
            &[Some(90.0), Some(40.0), None, None, None],
            &[("AAA".into(), 20), ("BBB".into(), 14)],
            SRC_LIVE, "the desk",
        );
        assert_eq!(row.split('\t').count(), m.len(), "{}", row);
        let cells: Vec<&str> = row.split('\t').collect();
        let at = |name: &str| cells[m.iter().position(|c| c == name).unwrap()];
        // THE ROOM'S RATE IS COUNTS OVER TUBE-SECONDS, not over the wall
        // clock: two tubes are two measurements of one number and do not
        // double the dose. 34 arrivals in 60 tube-seconds is 0.5666...
        assert_eq!(at("cps"), exact(34.0 / 60.0));
        // And the arrival rate at the machine is the other one, which is n
        // times it: 34 in 30 seconds of room.
        assert_eq!(at("cps_raw"), exact(34.0 / 30.0));
        assert_eq!(at("counts"), "34");
        assert_eq!(at("tubes"), "2");
        assert_eq!(at("per_tube"), "AAA=20,BBB=14");
        assert_eq!(at("site"), "the desk");
        // An empty window is empty, not zero -- the same rule the other
        // format keeps, and for the same reason.
        assert_eq!(at("cpm_300"), "");
    }

    /// THE MERGE IS NOT A TUBE, and `files` is where that is enforced: a
    /// report that let it through would count the room once per counter and
    /// once more for the merge of them.
    #[test]
    fn the_merged_log_is_never_mistaken_for_a_counter() {
        let dir = std::env::temp_dir()
            .join(format!("radbeeper-merged-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        for name in ["cpm-F48824B8207F7E-2026-09.tsv", "cpm-AA1122BB3344CC-2026-09.tsv",
                     "cpm-merged-2026-09.tsv", "cpm-merged-2026-10.tsv"] {
            fs::write(dir.join(name), "").unwrap();
        }
        let serials: Vec<String> = files(&dir).into_iter().map(|(s, _)| s).collect();
        assert_eq!(serials, vec!["AA1122BB3344CC", "F48824B8207F7E"]);
        // And it is reachable, by the one door that leads to it.
        let merged: Vec<String> = merged_files(&dir)
            .into_iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(merged, vec!["cpm-merged-2026-09.tsv", "cpm-merged-2026-10.tsv"]);
        let _ = fs::remove_dir_all(&dir);
    }

    /// Every one of these is what Python's `%g` prints for that value. They
    /// were taken from the Python, not reasoned about: the point of the
    /// function is to agree with it, so the test has to be its output.
    const G_CASES: &[(f64, &str)] = &[
        (3.0, "3"),
        (30.0, "30"),
        (300.0, "300"),
        (3000.0, "3000"),
        (0.5, "0.5"),
        (1.0, "1"),
        (10.0, "10"),
        (60.0, "60"),
        (30.1666666666, "30.1667"),
        (29.16111111, "29.1611"),
        (27.15, "27.15"),
        (4.0, "4"),
        (0.0, "0"),
        (2.5, "2.5"),
        (0.1, "0.1"),
        (100000.0, "100000"),
        (1234567.0, "1.23457e+06"),
        (1000000.0, "1e+06"),
        (123456789.0, "1.23457e+08"),
        (0.000123456789, "0.000123457"),
        (0.00001, "1e-05"),
        // The rounding boundary: six significant figures make this 1.00000e+06,
        // so C and Python switch to style e where the unrounded exponent
        // would have kept style f.
        (999999.5, "1e+06"),
        (999999.4, "999999"),
        (0.00009999999, "0.0001"),
    ];

    #[test]
    fn g_agrees_with_python() {
        for (v, want) in G_CASES {
            assert_eq!(&g(*v), want, "%g of {}", v);
        }
    }

    #[test]
    fn the_header_names_a_column_per_window_twice() {
        let h = header(&[3.0, 30.0, 300.0, 3000.0]);
        assert_eq!(
            h,
            "#time\tcps\tcounts\tseconds\tcpm_3\tcpm_30\tcpm_300\tcpm_3000\t\
             peak_3\tpeak_30\tpeak_300\tpeak_3000\tsrc\tsite"
        );
        assert!(h.starts_with('#'), "the header must sort above the rows");
        assert_eq!(columns(&h).len(), 14);
        assert_eq!(columns(&h)[0], "time");
    }

    #[test]
    fn a_row_is_tabs_and_the_time_comes_first() {
        let r = row(
            1_788_600_000.0,
            0.333,
            10,
            30.0,
            &[Some(20.0), None],
            &[Some(60.0), None],
            SRC_LIVE,
            "The bench",
        );
        // time, cps, counts, seconds, then one cpm per span, then one peak
        // per span, then src and site: four plus two plus two plus two.
        let cells: Vec<&str> = r.split('\t').collect();
        assert_eq!(cells.len(), 10);
        assert_eq!(cells[0], clock::stamp(1_788_600_000.0));
        assert_eq!(cells[1], "0.333");
        assert_eq!(cells[2], "10");
        assert_eq!(cells[3], "30");
        assert_eq!(cells[4], "20.0");
        assert_eq!(cells[5], "", "a window that was not full is empty");
        assert_eq!(cells[6], "60.0");
        assert_eq!(cells[7], "");
        assert_eq!(cells[8], SRC_LIVE);
        assert_eq!(cells[9], "The bench");
    }

    #[test]
    fn an_empty_field_is_not_a_zero() {
        // The single most important thing about this format, and the reason
        // the tab-visible screenshot exists in the README.
        let r = row(0.0, 0.0, 0, 30.0, &[None], &[None], SRC_LIVE, "");
        assert!(r.contains("\t\t"), "{}", r);
        let zero = row(0.0, 0.0, 0, 30.0, &[Some(0.0)], &[Some(0.0)], SRC_LIVE, "");
        assert!(zero.contains("0.0"));
        assert_ne!(r, zero);
    }

    #[test]
    fn a_row_goes_in_the_file_for_its_own_month() {
        let d = Path::new("/tmp");
        let name = path(1_788_600_000.0, d, Some("A1"));
        let stem = name.file_name().unwrap().to_str().unwrap();
        assert!(stem.starts_with("cpm-A1-"), "{}", stem);
        assert!(stem.ends_with(".tsv"));
        assert_eq!(stem.len(), "cpm-A1-2026-09.tsv".len());
        // A month later is a different file, with nothing to rename.
        assert_ne!(name, path(1_791_600_000.0, d, Some("A1")));
        // And no serial is not the same file as some serial.
        assert_ne!(name, path(1_788_600_000.0, d, None));
    }

    #[test]
    fn columns_are_matched_by_name_so_a_new_window_cannot_shift_a_row() {
        // THE BUG THIS EXISTS FOR. Adding a span inserts cpm_3000 after
        // cpm_300 and peak_3000 after peak_300, in the middle of the row,
        // twice. Padding on the right would slide src into a peak column.
        let old: Vec<String> = columns(&header(&[3.0, 300.0]));
        let new: Vec<String> = columns(&header(&[3.0, 300.0, 3000.0]));
        let cells = vec![
            "2026-09-04T12:00:00", "0.3", "9", "30",
            "20.0", "33.0", "60.0", "40.0", "live", "The bench",
        ];
        let out = align_row(&cells, &old, &new);
        let at = |n: &str| out[new.iter().position(|x| x == n).unwrap()].clone();
        assert_eq!(at("src"), "live", "src must not land in a peak column");
        assert_eq!(at("site"), "The bench");
        assert_eq!(at("cpm_300"), "33.0");
        assert_eq!(at("cpm_3000"), "", "a column that did not exist is empty");
        assert_eq!(at("peak_3000"), "");
    }

    /// A SECOND RADBEEPER MUST NOT ANSWER FOR THE FIRST. `--logs` exists so a
    /// test rig, or a service on another directory, can run beside the real
    /// one; the status file ignored it and wrote to the shared state
    /// directory regardless, so a test service stopping reported "stopped"
    /// into the running service's status while that service went on logging.
    /// `cat /var/lib/radbeeper/status` is what the init script tells people
    /// to read when nothing seems to be happening.
    #[test]
    fn a_status_is_written_where_it_was_told_and_nowhere_else() {
        let root = std::env::temp_dir().join(format!("rb-status-{}", std::process::id()));
        let (real, other) = (root.join("real"), root.join("other"));
        fs::create_dir_all(&real).unwrap();
        fs::create_dir_all(&other).unwrap();

        let p = write_status(&real, "monitoring /dev/ttyUSB0");
        assert_eq!(p, real.join("status"));
        write_status(&other, "stopped");

        let said = fs::read_to_string(real.join("status")).unwrap();
        assert!(said.contains("monitoring /dev/ttyUSB0"), "{}", said);
        assert!(!said.contains("stopped"), "the other one answered for it: {}", said);
        assert!(fs::read_to_string(other.join("status")).unwrap().contains("stopped"));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_slot_is_a_rows_identity() {
        assert_eq!(slot_of(0.0, 30.0), 0);
        assert_eq!(slot_of(29.9, 30.0), 0);
        assert_eq!(slot_of(30.0, 30.0), 1);
        assert_eq!(slot_of(-1.0, 30.0), -1, "floor, not truncate");
    }
}

/// Where logs and the status file live: the system directory when it can be
/// written to, the user's own when it cannot.
///
/// Probed rather than assumed, and in that order, because the service runs as
/// root at boot and a person running `radbeeper watch` does not -- and when
/// /var/lib/radbeeper is root:dialout with group write, both land in it.
///
/// /VAR/LIB, NOT /VAR/LOG. On Alpine desktops /var/log is commonly a tmpfs,
/// and it was on the machine these logs come from: every reboot emptied the
/// log, and only as much as the counter's flash still held came back. A
/// measurement record is state, not a log to be rotated away. /var/log is
/// still tried second, so an install that has only that keeps working.
pub fn state_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        PathBuf::from("/var/lib/radbeeper"),
        PathBuf::from("/var/log/radbeeper"),
        PathBuf::from(home).join(".local/share/radbeeper"),
    ];
    for d in candidates {
        if d.as_os_str().is_empty() {
            continue;
        }
        if fs::create_dir_all(&d).is_err() {
            continue;
        }
        let probe = d.join(".writable");
        if fs::File::create(&probe).is_ok() {
            let _ = fs::remove_file(&probe);
            return d;
        }
    }
    PathBuf::from(".")
}

/// A line saying what the service is doing, for a service that looks stuck.
///
/// IT WRITES WHERE IT IS TOLD, which it did not until a test service stopped
/// and reported itself into the REAL one's status file. This used `state_dir`
/// and ignored `--logs` entirely, so any second radbeeper -- a test rig, a
/// synthetic counter, a service on another log directory -- would overwrite
/// the running one's status on the way past. `cat /var/lib/radbeeper/status`
/// is what the init script tells people to read when nothing seems to be
/// happening, and it was answering for a process that had never touched the
/// counter.
pub fn write_status(directory: &Path, text: &str) -> PathBuf {
    let path = directory.join("status");
    if let Ok(mut f) = fs::File::create(&path) {
        let _ = writeln!(f, "{}  {}", clock::format(clock::now(), "%Y-%m-%d %H:%M:%S"), text);
    }
    path
}

/// What happened between two log lines, in constant space.
///
/// The peaks are why this exists. A row every thirty seconds carrying only
/// the averages as they stood at the moment of writing would miss a source
/// that came and went in between -- the single event most worth having
/// afterwards. A running maximum per window costs one comparison a second and
/// no memory that grows.
pub struct Interval {
    pub counts: u64,
    pub seconds: f64,
    pub peaks: Vec<Option<f64>>,
}

impl Interval {
    pub fn new(spans: usize) -> Interval {
        Interval { counts: 0, seconds: 0.0, peaks: vec![None; spans] }
    }

    pub fn reset(&mut self) {
        self.counts = 0;
        self.seconds = 0.0;
        for p in self.peaks.iter_mut() {
            *p = None;
        }
    }

    /// One sample: the raw count, each window as it stands, and how long the
    /// sample covers.
    ///
    /// `dt` is 1.0 from the heartbeat, which is a second by definition. It is
    /// NOT 1.0 for samples out of the counter's flash, where a recorded
    /// "second" measures 1.011 of ours -- and a column that says `seconds`
    /// has to mean seconds, or the cps beside it is a percent wrong for no
    /// visible reason.
    pub fn add(&mut self, counts: u32, averages: &[Option<f64>], dt: f64) {
        self.counts += counts as u64;
        self.seconds += dt;
        for (i, cpm) in averages.iter().enumerate() {
            if let (Some(v), Some(slot)) = (cpm, self.peaks.get_mut(i)) {
                if slot.is_none() || *v > slot.unwrap() {
                    *slot = Some(*v);
                }
            }
        }
    }

    /// Counts per ONE second, whatever the interval's length. The row spacing
    /// is never the divisor: cps means per second here as everywhere else.
    pub fn cps(&self) -> f64 {
        if self.seconds == 0.0 {
            0.0
        } else {
            self.counts as f64 / self.seconds
        }
    }
}

/// Appends rows to the dated log for their month.
///
/// Two things it watches for, both once per row and so once per thirty
/// seconds, against the two syscalls the row itself costs:
///
/// THE MONTH TURNING OVER, which is the whole of rotation.
///
/// THE FILE BEING REPLACED UNDERNEATH IT. A backfill merges by writing a new
/// file and renaming it over the old one, which leaves an appender holding a
/// descriptor onto an orphaned inode: it goes on writing, to nothing anybody
/// will ever read. Comparing the inode catches that and reopens.
pub struct Writer {
    /// The header this writer's file carries, and so the columns its rows are
    /// read back under. Held rather than recomputed because the merged log
    /// and the per-counter log are two formats over the same machinery -- the
    /// month rotation, the replaced-inode check and the one-row-per-slot rule
    /// are identical and the columns are not.
    head: String,
    dir: PathBuf,
    serial: Option<String>,
    every: f64,
    path: Option<PathBuf>,
    file: Option<fs::File>,
    ino: Option<u64>,
    last_slot: Option<i64>,
}

impl Writer {
    pub fn new(spans: &[f64], dir: PathBuf, serial: Option<String>, every: f64)
        -> Writer
    {
        Writer::under(header(spans), dir, serial, every)
    }

    /// The merged log's writer: the same file machinery, the other format.
    /// Its serial slot is `MERGED`, which no counter can have.
    pub fn merged(spans: &[f64], dir: PathBuf, every: f64) -> Writer {
        Writer::under(merged_header(spans), dir, Some(MERGED.to_string()), every)
    }

    fn under(head: String, dir: PathBuf, serial: Option<String>, every: f64)
        -> Writer
    {
        Writer {
            head,
            dir,
            serial,
            every,
            path: None,
            file: None,
            ino: None,
            last_slot: None,
        }
    }

    fn open(&mut self, path: &Path) -> std::io::Result<()> {
        self.close();
        let fresh = fs::metadata(path).map(|m| m.len() == 0).unwrap_or(true);
        let mut f = fs::OpenOptions::new().create(true).append(true).open(path)?;
        // A NEW HEADER WHEN THE WINDOWS CHANGED. A month's file outlives the
        // program that started it: a 3,30,300,3000 logger replaced mid-month
        // by a five-window one appended five-window rows under the four-window
        // header, and every peak column after the switch was read as the one
        // beside it. Both readers take the last header above a row as that
        // row's, so a second header is the whole of the migration.
        if fresh || last_header(path).as_deref() != Some(self.head.as_str()) {
            writeln!(f, "{}", self.head)?;
            f.flush()?;
        }
        // The slot already on disk, so a restart cannot append a second row
        // for a slot the previous run finished. That is exactly what happens
        // when a service comes back mid-interval: its first row would be a
        // short one covering a stretch the last run already wrote in full.
        let names = columns(&self.head);
        self.last_slot = read_table(path, &names)
            .last()
            .map(|(w, _)| slot_of(*w, self.every));
        self.ino = ino_of(path);
        self.path = Some(path.to_path_buf());
        self.file = Some(f);
        Ok(())
    }

    fn replaced(&self) -> bool {
        match (&self.path, self.ino) {
            (Some(p), Some(i)) => ino_of(p) != Some(i),
            _ => true,
        }
    }

    /// Append a row, unless its slot is already spoken for.
    ///
    /// One row per slot is the rule the whole format rests on, and it has to
    /// hold for the live logger too -- not only for the merge a backfill does.
    pub fn write(&mut self, when: f64, text: &str) -> std::io::Result<bool> {
        let want = path(when, &self.dir, self.serial.as_deref());
        if self.path.as_deref() != Some(want.as_path())
            || self.file.is_none()
            || self.replaced()
        {
            self.open(&want)?;
        }
        let slot = slot_of(when, self.every);
        if self.last_slot.map(|last| slot <= last).unwrap_or(false) {
            return Ok(false);
        }
        if let Some(f) = self.file.as_mut() {
            writeln!(f, "{}", text)?;
            f.flush()?;
        }
        self.last_slot = Some(slot);
        Ok(true)
    }

    pub fn close(&mut self) {
        self.file = None;
    }
}

/// The last header line in a log, which is the one its next row is read under.
fn last_header(path: &Path) -> Option<String> {
    let text = fs::read_to_string(path).ok()?;
    text.lines().rev().find(|l| l.starts_with('#')).map(str::to_string)
}

fn ino_of(path: &Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    fs::metadata(path).ok().map(|m| m.ino())
}

/// [(serial, from_time, name)] for every recorded move, oldest first.
pub fn read_sites(directory: &Path) -> Vec<(String, f64, String)> {
    let text = match fs::read_to_string(directory.join("sites.tsv")) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    let mut out = Vec::new();
    for line in text.lines() {
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let cells: Vec<&str> = line.split('\t').collect();
        if cells.len() < 3 {
            continue;
        }
        if let Some(when) = clock::parse_stamp(cells[1]) {
            out.push((cells[0].to_string(), when, cells[2].to_string()));
        }
    }
    out.sort_by(|a, b| (a.0.as_str(), a.1).partial_cmp(&(b.0.as_str(), b.1)).unwrap());
    out
}

/// Where that counter was at that moment.
///
/// Readings older than the first thing written down belong to the first place
/// we know about, not to nowhere: the counter was somewhere, and the earliest
/// record is the best evidence of where.
pub fn site_at(serial: &str, when: f64, sites: &[(String, f64, String)])
    -> Option<String>
{
    let mine: Vec<&(String, f64, String)> =
        sites.iter().filter(|r| r.0 == serial).collect();
    let first = mine.first()?;
    let mut current = *first;
    for row in &mine {
        if row.1 <= when {
            current = row;
        }
    }
    Some(current.2.clone())
}

#[cfg(test)]
mod writer_tests {
    use super::*;

    #[test]
    fn a_change_of_windows_mid_file_writes_a_new_header() {
        let dir = std::env::temp_dir().join(format!("rb-hdr-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let when = 1_789_000_000.0;
        let four = [3.0, 30.0, 300.0, 3000.0];
        let five = [3.0, 30.0, 300.0, 3000.0, 30000.0];
        let p = path(when, &dir, Some("A1"));
        let mut w = Writer::new(&four, dir.clone(), Some("A1".into()), 30.0);
        w.write(when, &format!("{}\tfour", clock::stamp(when))).unwrap();
        w.close();
        let mut w = Writer::new(&four, dir.clone(), Some("A1".into()), 30.0);
        w.write(when + 30.0, &format!("{}\tfour", clock::stamp(when + 30.0))).unwrap();
        w.close();
        let mut w = Writer::new(&five, dir.clone(), Some("A1".into()), 30.0);
        w.write(when + 60.0, &format!("{}\tfive", clock::stamp(when + 60.0))).unwrap();
        w.close();
        let text = fs::read_to_string(&p).unwrap();
        let heads: Vec<&str> = text.lines().filter(|l| l.starts_with('#')).collect();
        assert_eq!(heads, vec![header(&four).as_str(), header(&five).as_str()],
                   "a restart with the same windows must not repeat the header");
        let _ = fs::remove_dir_all(&dir);
    }
}
