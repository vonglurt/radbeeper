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
        out.push((serial, directory.join(&name)));
    }
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
/// root at boot and a person running `radbeeper service` by hand does not.
pub fn state_dir() -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
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
pub fn write_status(text: &str) -> PathBuf {
    let path = state_dir().join("status");
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
    spans: Vec<f64>,
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
        Writer {
            spans: spans.to_vec(),
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
        if fresh {
            writeln!(f, "{}", header(&self.spans))?;
            f.flush()?;
        }
        // The slot already on disk, so a restart cannot append a second row
        // for a slot the previous run finished. That is exactly what happens
        // when a service comes back mid-interval: its first row would be a
        // short one covering a stretch the last run already wrote in full.
        let names = columns(&header(&self.spans));
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
