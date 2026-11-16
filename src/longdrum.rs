// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
// The long drums: a line of spectrum for a whole day, and one for a whole
// month, made from the count log that is already on disk.
//
// WHY THE COUNT LOG AND NOT THE SECONDS. The drum on the screen is made of
// seconds, and the seconds are in the service's memory: twelve minutes of
// paper, gone at a restart. A rhythm that comes round once an hour, or once
// a day, is not in twelve minutes of anything. But the count log has a row
// every thirty seconds for as long as the counter has been plugged in, and a
// period of two minutes or longer is as visible in thirty-second counts as
// in one-second ones. So a day's line is the spectrum of that day's 2,880
// rows, a month's is the spectrum of the month in five-minute sums, and
// both exist for every day and month there is a log for -- including the
// ones from before this was written.
//
// A LINE THAT IS NOT FINISHED IS STILL A LINE. Today's is made of what today
// has so far and says so (`whole` is false): it reaches only the periods
// that have had time to come round, and it fills in leftwards as the day
// goes on. Nothing waits for midnight to be drawn.
//
// A BREAK IN THE RECORD IS LEFT AS A BREAK. Thirty seconds with no row are
// thirty seconds nobody counted, not thirty seconds of nothing: they are
// left out of the mean and out of the transform, and the line says how much
// of its span it was made from. A day with less than a quarter of itself is
// no line at all.
use crate::{clock, log};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

/// How many bands a line is folded into, at most.
pub const BANDS: usize = 192;
/// The count log's own spacing, and a day line's bin: seconds.
pub const DAY_BIN: f64 = 30.0;
/// A month line's bin: five minutes of the count log, summed.
pub const MONTH_BIN: f64 = 300.0;
/// The least of its span a line may be made from.
pub const LEAST_COVERED: f32 = 0.25;
/// The slowest thing a line is asked about: what comes round this many times
/// in it.
///
/// ONCE OR TWICE IN A LINE IS A DRIFT, NOT A RHYTHM. A room whose rate was
/// higher in the evening than the morning has all of that in the first two
/// places of the transform, and the taper smears it across them: on the
/// first papers every day had a white bar down its left edge, real logs and
/// made-up ones alike, which said only that days differ. Something has to
/// come round three times before a line calls it a period, so a day line
/// begins at eight hours and a month line at ten days -- and what a day does
/// once a day is on the month paper, where it comes round thirty times.
pub const SLOWEST: usize = 3;
/// The fewest bins with something in them that are worth a transform.
const LEAST_BINS: usize = 32;

/// Which paper a line belongs on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Paper {
    /// A line a local day, from midnight to midnight.
    Day,
    /// A line a calendar month, which is one file of the count log.
    Month,
}

impl Paper {
    pub fn bin(self) -> f64 {
        match self {
            Paper::Day => DAY_BIN,
            Paper::Month => MONTH_BIN,
        }
    }

    /// The start of the span `t` is in.
    pub fn start(self, t: f64) -> f64 {
        match self {
            Paper::Day => clock::day_start(t),
            Paper::Month => {
                let y: i32 = clock::format(t, "%Y").parse().unwrap_or(1970);
                let m: i32 = clock::format(t, "%m").parse().unwrap_or(1);
                clock::from_parts(y, m, 1, 0, 0, 0).unwrap_or(t)
            }
        }
    }

    /// The start of the span after the one that starts at `start`.
    pub fn next(self, start: f64) -> f64 {
        match self {
            // A day and a half on from midnight is in tomorrow whatever the
            // clocks did tonight.
            Paper::Day => clock::day_start(start + 129_600.0),
            Paper::Month => Paper::Month.start(start + 32.0 * 86400.0),
        }
    }

    /// The start of the span before it.
    pub fn before(self, start: f64) -> f64 {
        self.start(start - 43200.0)
    }

    /// Where its finished lines are kept.
    pub fn path(self, directory: &Path) -> PathBuf {
        directory.join(match self {
            Paper::Day => "drum-day.tsv",
            Paper::Month => "drum-month.tsv",
        })
    }
}

/// One line of a long drum.
#[derive(Clone, Debug, PartialEq)]
pub struct LongRow {
    /// When its span began, and how many seconds of the span it reaches --
    /// the whole of it, or as far as the record goes.
    pub start: f64,
    pub seconds: f64,
    /// How much of those seconds had a row in the log: 1.0 is all of them.
    pub covered: f32,
    /// True for a span that is over; false for the one still being counted.
    pub whole: bool,
    /// The average count over it, in counts a second for one tube.
    pub cps: f64,
    /// (period in seconds, power), the longest period first. 1.0 is what
    /// chance puts in a band.
    pub bands: Vec<(f32, f32)>,
}

/// The count log, read once: every row of every counter as (the middle of
/// its interval, counts, seconds), in order of time.
pub struct Counts {
    rows: Vec<(f64, f64, f64)>,
}

impl Counts {
    /// Every row in `directory` whose month could hold a moment from `since`
    /// to `until`.
    pub fn load(directory: &Path, since: f64, until: f64) -> Counts {
        let first = clock::format(since - 86400.0, "%Y-%m");
        let last = clock::format(until + 86400.0, "%Y-%m");
        let names = vec!["counts".to_string(), "seconds".to_string()];
        let mut rows = Vec::new();
        for (_serial, path) in log::files(directory) {
            let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
            let month = name.trim_end_matches(".tsv").rsplitn(3, '-').take(2).collect::<Vec<_>>();
            let month = format!("{}-{}", month.get(1).unwrap_or(&""), month.first().unwrap_or(&""));
            if month.as_str() < first.as_str() || month.as_str() > last.as_str() {
                continue;
            }
            for (when, cells) in log::read_table(&path, &names) {
                let (Ok(counts), Ok(seconds)) = (cells[0].parse::<f64>(), cells[1].parse::<f64>())
                else {
                    continue;
                };
                if seconds > 0.0 {
                    // A row is stamped when it was written, which is the end
                    // of what it counted.
                    rows.push((when - seconds / 2.0, counts, seconds));
                }
            }
        }
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        Counts { rows }
    }

    pub fn from_rows(mut rows: Vec<(f64, f64, f64)>) -> Counts {
        rows.sort_by(|a, b| a.0.total_cmp(&b.0));
        Counts { rows }
    }

    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The first and last moments there is a row for.
    pub fn reach(&self) -> Option<(f64, f64)> {
        Some((self.rows.first()?.0, self.rows.last()?.0))
    }

    /// Counts a second for one tube, in bins of `bin` seconds from `start`
    /// up to `end`; None for a bin nobody counted in.
    ///
    /// EVERY COUNTER'S ROWS TOGETHER, counts over seconds: two tubes in the
    /// room are twice the counts in twice the tube-seconds, so the figure is
    /// the room's rate for one tube however many were listening.
    pub fn binned(&self, start: f64, end: f64, bin: f64) -> Vec<Option<f64>> {
        let n = ((end - start) / bin).ceil().max(0.0) as usize;
        let mut sums = vec![(0.0f64, 0.0f64); n];
        // A ROW IS DEALT OUT OVER THE BINS IT LIES ACROSS, by how much of it
        // lies in each. A row is thirty of the counter's seconds, and the
        // counter's second is a little over one of ours: the rows of a real
        // log come every thirty-one seconds or so, and dropped whole into
        // thirty-second bins by their middles they leave a bin with nothing
        // in it every quarter of an hour. That read as a break in the record
        // where there was none -- the window filled its long averages from
        // the log back to the first such hole and stopped (2026-11-16).
        // Rows longer than this are not looked for before `start`.
        let from = self.rows.partition_point(|r| r.0 < start - 3600.0);
        for &(mid, counts, seconds) in &self.rows[from..] {
            let (a, b) = (mid - seconds / 2.0, mid + seconds / 2.0);
            if a >= end {
                break;
            }
            if b <= start {
                continue;
            }
            let first = (((a - start) / bin).floor().max(0.0)) as usize;
            let last = ((((b - start) / bin).ceil()) as usize).min(n);
            for (k, cell) in sums.iter_mut().enumerate().take(last).skip(first) {
                let (lo, hi) = (start + k as f64 * bin, start + (k + 1) as f64 * bin);
                let share = (b.min(hi) - a.max(lo)).max(0.0);
                cell.0 += counts * share / seconds;
                cell.1 += share;
            }
        }
        // A bin is counted when most of it was: a sliver of a row's edge is
        // not a measurement of the bin.
        sums.iter().map(|(c, s)| (*s >= bin / 2.0).then(|| c / s)).collect()
    }
}

/// The spectrum of a run of bins, folded into bands.
///
/// The mean is taken off and a Hann taper put on, as for every other
/// spectrum here, and each power is held against what chance would put
/// there: the scatter of the bins -- half the mean square of the change from
/// one to the next, see `analysis::Waterfall` -- times the taper's energy
/// over the bins that had something in them. Flat reads 1.0.
///
/// FOLDED BY THE MEAN, NOT THE LOUDEST. A day has 720 periods between two
/// minutes and itself and the paper has a couple of hundred places to put
/// them. The loudest of the periods that share a place is high by
/// construction -- it is the tallest of several draws -- and a row of those
/// is a wall. Their mean is still 1.0 where there is nothing, and a period
/// that is really there lifts it.
///
/// The bands are even steps of the logarithm of the period, from a third of
/// the run's own length (see `SLOWEST`) down to four bins. At the long end the periods are further
/// apart than the bands and some bands hold none: those are left out, and
/// each band that is given carries the period it stands at.
pub fn spectrum_of(values: &[Option<f64>], bin: f64) -> Option<(f64, f32, Vec<(f32, f32)>)> {
    let n = values.len();
    let have: Vec<f64> = values.iter().flatten().copied().collect();
    if n < LEAST_BINS || have.len() < LEAST_BINS {
        return None;
    }
    let covered = have.len() as f32 / n as f32;
    let mean = have.iter().sum::<f64>() / have.len() as f64;
    // The scatter from neighbours that are both there: a break is not a
    // change.
    let (mut steps, mut pairs) = (0.0f64, 0usize);
    for w in values.windows(2) {
        if let (Some(a), Some(b)) = (w[0], w[1]) {
            steps += (b - a) * (b - a);
            pairs += 1;
        }
    }
    let variance = if pairs > 0 { steps / (2.0 * pairs as f64) } else { 0.0 };
    let (mut shaped, mut energy) = (vec![0.0f64; n], 0.0f64);
    for (j, v) in values.iter().enumerate() {
        if let Some(v) = v {
            let t = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * j as f64 / (n - 1) as f64).cos();
            shaped[j] = (v - mean) * t;
            energy += t * t;
        }
    }
    let level = variance * energy;
    let turn: Vec<(f64, f64)> = (0..n)
        .map(|j| (2.0 * std::f64::consts::PI * j as f64 / n as f64).sin_cos())
        .collect();
    let top = n / 4;
    let span = (top as f64 / SLOWEST as f64).ln().max(1e-9);
    let mut sums = vec![(0.0f64, 0.0f64, 0u32); BANDS];
    for k in SLOWEST..=top {
        let (mut re, mut im, mut at) = (0.0f64, 0.0f64, 0usize);
        for v in &shaped {
            let (sin, cos) = turn[at];
            re += v * cos;
            im -= v * sin;
            at += k;
            if at >= n {
                at -= n;
            }
        }
        let power = if level > 0.0 { (re * re + im * im) / level } else { 0.0 };
        let band = (((k as f64 / SLOWEST as f64).ln() / span) * BANDS as f64) as usize;
        let cell = &mut sums[band.min(BANDS - 1)];
        cell.0 += power;
        cell.1 += (n as f64 * bin / k as f64).ln();
        cell.2 += 1;
    }
    let bands = sums
        .iter()
        .filter(|c| c.2 > 0)
        .map(|c| ((c.1 / c.2 as f64).exp() as f32, (c.0 / c.2 as f64) as f32))
        .collect();
    Some((mean, covered, bands))
}

/// The line for the span that starts at `start`, as far as `now`; None where
/// the record has too little of it.
///
/// A LINE BEGINS WHERE ITS RECORD BEGINS AND ENDS WHERE IT ENDS. A counter
/// plugged in at a quarter to eleven has a line for today made of what came
/// after a quarter to eleven: the empty morning is cut off the front, not
/// carried as a break, and the line reaches the periods that stretch has had
/// time for. `covered` is still against the whole span, so a line made of an
/// afternoon says it is an afternoon; the least a line may be made from is
/// asked of the stretch it was actually cut to.
pub fn row(counts: &Counts, paper: Paper, start: f64, now: f64) -> Option<LongRow> {
    let end = paper.next(start);
    let whole = end <= now;
    let upto = if whole { end } else { now };
    let values = counts.binned(start, upto, paper.bin());
    let first = values.iter().position(|v| v.is_some())?;
    let last = values.iter().rposition(|v| v.is_some())?;
    let cut = &values[first..=last];
    let (cps, within, bands) = spectrum_of(cut, paper.bin())?;
    if within < LEAST_COVERED {
        return None;
    }
    let full = ((end - start) / paper.bin()).max(1.0);
    let have = cut.iter().flatten().count() as f64;
    Some(LongRow {
        start,
        seconds: cut.len() as f64 * paper.bin(),
        covered: (have / full).min(1.0) as f32,
        whole,
        cps,
        bands,
    })
}

fn header(bands: &[(f32, f32)]) -> String {
    let mut head: Vec<String> =
        ["time", "seconds", "covered", "cps"].iter().map(|s| s.to_string()).collect();
    head.extend(bands.iter().map(|b| format!("p{}", log::g(b.0 as f64))));
    format!("#{}", head.join("\t"))
}

/// The finished lines a paper has on disk, oldest first.
pub fn read(directory: &Path, paper: Paper) -> Vec<LongRow> {
    let Ok(text) = fs::read_to_string(paper.path(directory)) else { return Vec::new() };
    let mut periods: Vec<f32> = Vec::new();
    let mut out: Vec<LongRow> = Vec::new();
    for line in text.lines() {
        if let Some(head) = line.strip_prefix('#') {
            periods = head
                .split('\t')
                .skip(4)
                .filter_map(|c| c.strip_prefix('p')?.parse().ok())
                .collect();
            continue;
        }
        let cells: Vec<&str> = line.split('\t').collect();
        if cells.len() != 4 + periods.len() {
            continue;
        }
        let Some(start) = clock::parse_stamp(cells[0]) else { continue };
        let (Ok(seconds), Ok(covered), Ok(cps)) =
            (cells[1].parse(), cells[2].parse(), cells[3].parse())
        else {
            continue;
        };
        let powers: Vec<f32> = cells[4..].iter().filter_map(|c| c.parse().ok()).collect();
        if powers.len() != periods.len() {
            continue;
        }
        out.retain(|r| r.start != start);
        out.push(LongRow {
            start, seconds, covered, whole: true, cps,
            bands: periods.iter().copied().zip(powers).collect(),
        });
    }
    out.sort_by(|a, b| a.start.total_cmp(&b.start));
    out
}

/// Write down every finished line the count log can give and the paper's
/// own log does not have, and say how many that was.
///
/// THE LOG OF LINES IS A COPY, AND THE COUNT LOG IS THE RECORD. Every line
/// here can be made again from the counts, so nothing is lost if this file
/// is; it is kept so a window opening on a year of counts reads a few
/// hundred lines and not a few million rows. Only whoever holds the port
/// writes it -- the service, at its start and as each day ends -- and a line
/// is written once, when its span is over.
pub fn keep(directory: &Path, paper: Paper, now: f64) -> std::io::Result<usize> {
    let have: Vec<f64> = read(directory, paper).iter().map(|r| r.start).collect();
    let all = Counts::load(directory, 0.0, now);
    let Some((first, _)) = all.reach() else { return Ok(0) };
    let mut start = paper.start(first);
    let mut wrote = 0usize;
    let path = paper.path(directory);
    while paper.next(start) <= now {
        if !have.contains(&start) {
            if let Some(r) = row(&all, paper, start, now) {
                let head = header(&r.bands);
                let last = fs::read_to_string(&path)
                    .ok()
                    .and_then(|t| t.lines().rev().find(|l| l.starts_with('#')).map(str::to_string));
                let mut f = fs::OpenOptions::new().create(true).append(true).open(&path)?;
                if last.as_deref() != Some(head.as_str()) {
                    writeln!(f, "{}", head)?;
                }
                let mut cells = vec![
                    clock::stamp(r.start),
                    log::g(r.seconds),
                    format!("{:.3}", r.covered),
                    format!("{:.4}", r.cps),
                ];
                cells.extend(r.bands.iter().map(|b| format!("{:.2}", b.1)));
                writeln!(f, "{}", cells.join("\t"))?;
                wrote += 1;
            }
        }
        let next = paper.next(start);
        if next <= start {
            break;
        }
        start = next;
    }
    Ok(wrote)
}

/// How far back a paper looks for lines: spans, counted from now.
pub fn look_back(paper: Paper) -> usize {
    match paper {
        Paper::Day => 120,
        Paper::Month => 36,
    }
}

/// The newest `lines` lines a paper has, newest first: the one still being
/// counted, if it is yet a line, then the finished ones before it.
///
/// A BREAK IS CLOSED UP, NOT DRAWN. A counter that was away for three weeks
/// left three weeks of nothing, and a paper that kept a track for each empty
/// day was most of a screen of ruled ground with one line at its foot. So a
/// day or a month with no line is passed over and the next one that has a
/// line sits directly above; every line carries the day it began, and that
/// is where the break can be read. The search goes back `look_back` spans
/// and no further.
///
/// Every entry is `Some`; the type is kept so a caller written for empty
/// tracks still reads it.
///
/// A finished line comes from the paper's own log where it is there and
/// from the counts where it is not, so a reader that may write nothing
/// still shows everything.
pub fn rows(directory: &Path, paper: Paper, now: f64, lines: usize) -> Vec<Option<LongRow>> {
    let mut starts = Vec::new();
    let mut at = paper.start(now);
    for _ in 0..look_back(paper) {
        starts.push(at);
        at = paper.before(at);
    }
    let kept = read(directory, paper);
    // The counts are read once, and only if some span in reach has no kept
    // line: a month of days already written down costs no reading at all.
    let missing = starts.iter().any(|s| !kept.iter().any(|r| r.start == *s));
    let counts = if missing {
        Counts::load(directory, *starts.last().unwrap_or(&now), now)
    } else {
        Counts::from_rows(Vec::new())
    };
    let mut out = Vec::with_capacity(lines);
    for s in &starts {
        if out.len() >= lines {
            break;
        }
        let line = kept
            .iter()
            .find(|r| r.start == *s)
            .cloned()
            .or_else(|| row(&counts, paper, *s, now));
        if line.is_some() {
            out.push(line);
        }
    }
    out
}

/// How the room for the drums is shared out among the papers that have
/// something to show: lines for the live drum, the day drum and the month
/// drum, in that order.
///
/// THE LIVE DRUM GIVES WAY AND IS NEVER SQUEEZED OUT. With nothing else to
/// show it has every line. When the day drum has a line it takes room at the
/// top, up to a third, and the live drum's history is cut short to make it;
/// the month drum then takes up to a quarter of what the day drum had. Each
/// paper keeps at least two lines once it is shown at all, so a line always
/// has a neighbour to be read against, and none is given more lines than it
/// has to show.
pub fn share(lines: usize, live: usize, days: usize, months: usize) -> (usize, usize, usize) {
    let live_min = 4.min(lines);
    let mut day = if days == 0 { 0 } else { days.min((lines / 3).max(2)) };
    let mut month = if months == 0 { 0 } else { months.min((lines / 8).max(2)) };
    while day + month + live_min > lines && (day > 0 || month > 0) {
        if month > 0 && (month >= day || day == 0) {
            month -= 1;
        } else {
            day -= 1;
        }
    }
    (live.min(lines - day - month), day, month)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sample;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("radbeeper-long-{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// The band of a line nearest a period, as (period, power).
    fn near(r: &LongRow, period: f32) -> (f32, f32) {
        *r.bands
            .iter()
            .min_by(|a, b| (a.0 / period).ln().abs().total_cmp(&(b.0 / period).ln().abs()))
            .unwrap()
    }

    fn loudest(r: &LongRow) -> (f32, f32) {
        *r.bands.iter().max_by(|a, b| a.1.total_cmp(&b.1)).unwrap()
    }

    /// A line as a strip of characters, hotter to the right of the list: what
    /// a test prints so a person can look at the sample month.
    fn strip(r: &Option<LongRow>, width: usize, long: f32, short: f32) -> String {
        let Some(r) = r else { return " ".repeat(width) };
        let mut cells = vec![0.0f32; width];
        for (p, v) in &r.bands {
            let x = ((long / p).ln() / (long / short).ln() * width as f32) as isize;
            if x >= 0 && (x as usize) < width {
                cells[x as usize] = cells[x as usize].max(*v);
            }
        }
        cells
            .iter()
            .map(|v| match *v {
                v if v <= 0.0 => ' ',
                v if v < 2.0 => '.',
                v if v < 4.0 => ':',
                v if v < 8.0 => '+',
                v if v < 16.0 => '*',
                _ => '#',
            })
            .collect()
    }

    #[test]
    fn decay_alone_is_flat_on_a_day_line() {
        let dir = tmp("flat");
        let now = clock::day_start(1_790_000_000.0) + 3600.0;
        sample::counts(&dir, "AAAAAAAAAAAAAA", now - 3.0 * 86400.0, now, 0.7, &[], &[], 11);
        let lines = rows(&dir, Paper::Day, now, 3);
        let yesterday = lines[1].as_ref().expect("a whole day of counts is a line");
        assert!(yesterday.whole && yesterday.covered > 0.99);
        assert!((yesterday.cps - 0.7).abs() < 0.02, "{}", yesterday.cps);
        let mean = yesterday.bands.iter().map(|b| b.1).sum::<f32>() / yesterday.bands.len() as f32;
        assert!((0.7..1.3).contains(&mean), "flat should read 1.0, read {}", mean);
        // Out past the long end a band is one draw; where bands hold several
        // periods their mean stays near 1.
        let short: Vec<f32> = yesterday.bands.iter().filter(|b| b.0 < 1800.0).map(|b| b.1).collect();
        assert!(short.iter().all(|p| *p < 4.0), "a wall where there should be a floor");
    }

    #[test]
    fn something_that_comes_round_every_hour_is_a_stripe_on_every_day() {
        let dir = tmp("hour");
        let now = clock::day_start(1_790_000_000.0) + 2.0 * 3600.0;
        sample::counts(&dir, "AAAAAAAAAAAAAA", now - 6.0 * 86400.0, now, 0.7, &[(3600.0, 0.10)], &[], 5);
        let lines = rows(&dir, Paper::Day, now, 6);
        for line in &lines[1..5] {
            let r = line.as_ref().unwrap();
            let (period, power) = loudest(r);
            assert!((period / 3600.0).ln().abs() < 0.05, "the stripe is at {} s", period);
            assert!(power > 20.0, "an hourly tenth should stand far over chance: {}", power);
            assert!(near(r, 600.0).1 < 4.0);
        }
    }

    #[test]
    fn todays_line_is_drawn_before_today_is_over() {
        let dir = tmp("partial");
        let now = clock::day_start(1_790_000_000.0) + 5.0 * 3600.0;
        sample::counts(&dir, "AAAAAAAAAAAAAA", now - 86400.0, now, 0.7, &[(900.0, 0.2)], &[], 9);
        let lines = rows(&dir, Paper::Day, now, 2);
        let today = lines[0].as_ref().expect("five hours is a line");
        assert!(!today.whole);
        assert!((today.seconds - 5.0 * 3600.0).abs() < 61.0);
        assert!((today.covered - 5.0 / 24.0).abs() < 0.01, "five hours of a day: {}", today.covered);
        assert!(today.bands.iter().all(|b| b.0 <= 5.0 * 3600.0 / 3.0 + 1.0),
                "a line reaches a third of its own length and no further");
        assert!((loudest(today).0 / 900.0).ln().abs() < 0.08);
        // And a quarter of an hour into a day is too little to transform.
        let early = clock::day_start(now) + 600.0;
        assert!(row(&Counts::load(&dir, 0.0, early), Paper::Day, clock::day_start(early), early).is_none());
    }

    #[test]
    fn a_break_is_left_out_and_a_day_nobody_counted_is_no_line() {
        let dir = tmp("gap");
        let now = clock::day_start(1_790_000_000.0) + 3600.0;
        let day = |k: f64| clock::day_start(now) - k * 86400.0;
        // Nothing at all three days back; the second half missing two back.
        let gaps = [(day(3.0), day(2.0)), (day(2.0) + 43200.0, day(1.0))];
        sample::counts(&dir, "AAAAAAAAAAAAAA", now - 5.0 * 86400.0, now, 0.7, &[(3600.0, 0.15)], &gaps, 3);
        let lines = rows(&dir, Paper::Day, now, 5);
        let half = lines[2].as_ref().expect("half a day is still a line");
        assert_eq!(half.start, day(2.0));
        assert!((half.covered - 0.5).abs() < 0.02, "{}", half.covered);
        assert!((loudest(half).0 / 3600.0).ln().abs() < 0.08, "and the hour is still in it");
        // The day nobody counted is passed over: the line above the half day
        // is the day before the empty one, and its date says so.
        assert_eq!(lines[3].as_ref().map(|r| r.start), Some(day(4.0)));
        assert!(lines.iter().all(|l| l.is_some()));
        assert_eq!(lines.len(), 5, "today so far, three whole days and the first afternoon");
    }

    #[test]
    fn a_counter_plugged_in_this_morning_has_a_line_today() {
        let dir = tmp("late");
        let now = clock::day_start(1_790_000_000.0) + 11.5 * 3600.0;
        // Nothing before a quarter to eleven; three quarters of an hour since.
        sample::counts(&dir, "AAAAAAAAAAAAAA", now - 2700.0, now, 0.7, &[(300.0, 0.3)], &[], 8);
        let today = rows(&dir, Paper::Day, now, 1)[0].clone().expect("forty-five minutes is a line");
        assert!((today.seconds - 2700.0).abs() < 61.0, "{}", today.seconds);
        assert!(today.covered < 0.05, "and it says how little of the day it is");
        assert!(today.bands.iter().all(|b| b.0 <= 900.0 + 1.0));
        assert!((loudest(&today).0 / 300.0).ln().abs() < 0.12, "{:?}", loudest(&today));
    }

    #[test]
    fn rows_a_little_over_thirty_seconds_apart_leave_no_holes() {
        // What a real counter writes: a row every 30.6 seconds, each of 30.6.
        let rows: Vec<(f64, f64, f64)> = (0..600)
            .map(|k| (1_000_000.0 + 15.3 + k as f64 * 30.6, 21.0, 30.6))
            .collect();
        let end = 1_000_000.0 + 600.0 * 30.6;
        let bins = Counts::from_rows(rows).binned(1_000_000.0, end, 30.0);
        assert!(bins.iter().all(|b| b.is_some()), "a hole where the record has none");
        let total: f64 = bins.iter().flatten().map(|cps| cps * 30.0).sum();
        assert!((total - 600.0 * 21.0).abs() < 25.0, "and the counts are all still there: {}", total);
        assert!(bins.iter().flatten().all(|cps| (cps - 21.0 / 30.6).abs() < 1e-9));
    }

    #[test]
    fn two_tubes_are_one_room() {
        let dir = tmp("pair");
        let now = clock::day_start(1_790_000_000.0) + 3600.0;
        sample::counts(&dir, "AAAAAAAAAAAAAA", now - 2.0 * 86400.0, now, 0.7, &[], &[], 1);
        sample::counts(&dir, "BBBBBBBBBBBBBB", now - 2.0 * 86400.0, now, 0.7, &[], &[], 2);
        let r = rows(&dir, Paper::Day, now, 2)[1].clone().unwrap();
        assert!((r.cps - 0.7).abs() < 0.02, "the rate for one tube, not for two: {}", r.cps);
    }

    /// THE SAMPLE MONTH, WHICH IS ALSO THE PICTURE. Forty-five days of counts
    /// at an ordinary background rate with three things in them that are not
    /// decay: something every twenty minutes for the whole of it, a daily
    /// rise and fall, and a week in the middle with something every hour.
    /// `cargo test sample_month -- --nocapture` prints both papers.
    #[test]
    fn a_sample_month_shows_its_days_and_its_months() {
        let dir = tmp("month");
        // The ninth of a month, so the paper has a whole month behind it.
        let now = Paper::Month.start(1_790_000_000.0) + 8.0 * 86400.0 + 7.0 * 3600.0;
        let begin = now - 45.0 * 86400.0;
        let week = (now - 25.0 * 86400.0, now - 18.0 * 86400.0);
        sample::counts(&dir, "AAAAAAAAAAAAAA", begin, now, 0.7,
                       &[(1200.0, 0.06), (86400.0, 0.05)], &[], 21);
        // The hourly week, as a second source laid over the same days.
        sample::counts(&dir, "BBBBBBBBBBBBBB", week.0, week.1, 0.7, &[(3600.0, 0.25)], &[], 22);

        let days = rows(&dir, Paper::Day, now, 30);
        println!("\nthe day paper: a line a day, today at the foot; 8 h at the left, 2 min at the right");
        for line in days.iter().rev() {
            let date = line.as_ref().map(|r| clock::format(r.start, "%d %b")).unwrap_or_default();
            println!("{:>7} |{}|", date, strip(line, 96, 28800.0, 120.0));
        }
        assert_eq!(days.len(), 30);
        assert!(days.iter().all(|d| d.is_some()), "forty-five days of counts are thirty lines");
        assert!(!days[0].as_ref().unwrap().whole);
        // Twenty minutes is in every whole day. THREE TIMES CHANCE, NOT SIX:
        // through the hourly week a second tube is counting beside the
        // first, and its counts have no twenty-minute rhythm in them, so the
        // room's rhythm is half as deep and its power a quarter. Six passed
        // where a day is cut at Pacific midnight and failed at UTC's, where
        // the week's days fall differently (17 Aug read 4.95) -- which is how
        // this was found, on the first machine that was not on the bench's
        // clock.
        for d in days[1..].iter().flatten() {
            assert!(near(d, 1200.0).1 > 3.0, "{}: {}", clock::format(d.start, "%d %b"), near(d, 1200.0).1);
        }
        // The hour is in the days of that week and not in the days outside it.
        let hourly: Vec<bool> = days[1..].iter().flatten().map(|d| near(d, 3600.0).1 > 8.0).collect();
        assert!(hourly.iter().filter(|h| **h).count() >= 5, "{:?}", hourly);
        assert!(!hourly[0] && !hourly[1], "the newest days are outside the week");

        let months = rows(&dir, Paper::Month, now, 3);
        println!("\nthe month paper: a line a month; 10 d at the left, 20 min at the right");
        for line in months.iter().rev() {
            let date = line.as_ref().map(|r| clock::format(r.start, "%b %Y")).unwrap_or_default();
            println!("{:>9} |{}|", date, strip(line, 96, 10.0 * 86400.0, 1200.0));
        }
        let last = months[1].as_ref().expect("the month before this one is whole");
        assert!(last.whole);
        assert!(near(last, 86400.0).1 > 10.0, "the daily rise and fall: {}", near(last, 86400.0).1);
        assert!(near(last, 1200.0).1 > 3.0, "twenty minutes, at the month's short end");
        assert!(months[0].as_ref().map(|m| !m.whole).unwrap_or(false), "this month is still being counted");

        // What the service writes down, and that reading it back is the same paper.
        assert_eq!(keep(&dir, Paper::Day, now).unwrap(), 45);
        assert_eq!(keep(&dir, Paper::Day, now).unwrap(), 0, "a line is written once");
        assert!(keep(&dir, Paper::Month, now).unwrap() >= 1);
        let again = rows(&dir, Paper::Day, now, 30);
        for (a, b) in days[1..].iter().zip(&again[1..]) {
            let (a, b) = (a.as_ref().unwrap(), b.as_ref().unwrap());
            assert_eq!(a.start, b.start);
            assert_eq!(a.bands.len(), b.bands.len());
            assert!(a.bands.iter().zip(&b.bands).all(|(x, y)| (x.1 - y.1).abs() < 0.006));
        }
        assert!(log::files(&dir).len() >= 3, "and the lines' own logs are not taken for counters");
    }

    #[test]
    fn the_room_is_shared_out_as_the_papers_fill() {
        // Nothing but the live drum: it has the paper.
        assert_eq!(share(96, 96, 0, 0), (96, 0, 0));
        // One day line appears, with a neighbour's room beside it.
        assert_eq!(share(96, 96, 1, 0), (95, 1, 0));
        // A month of days: up to a third of the paper, and the live drum cut short.
        assert_eq!(share(96, 96, 30, 0), (66, 30, 0));
        // And months as well.
        assert_eq!(share(96, 96, 30, 6), (60, 30, 6));
        // A small paper keeps the live drum and gives what is left.
        let (live, day, month) = share(8, 96, 30, 6);
        assert_eq!(live + day + month, 8);
        assert!(live >= 4 && day >= 2);
        assert_eq!(share(3, 96, 30, 6), (3, 0, 0));
    }
}
