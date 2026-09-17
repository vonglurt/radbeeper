// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
//
// `export`: index.html and random.html, out of the logs.
//
// BYTE-FOR-BYTE THE PYTHON'S, like the log format. The pages are generated
// from files a fork publishes, and a page that changes by a character every
// time a different binary builds it is a diff in every commit the workflow
// makes. tests/test_differential.py runs both exports on the same logs and
// compares the files.
//
// WHAT THAT TAKES, beyond writing the same strings:
//
//   * the same float arithmetic in the same order, because every coordinate
//     in the charts is printed to a tenth and one ulp can move a rounding;
//   * `pow`, `exp` and `log10` from libm, as the Python calls them, rather
//     than whatever LLVM would rewrite `powf(x, 2.0)` into;
//   * CPython's compensated `sum()` for the one float sum the Python does
//     with `sum`, which since 3.12 is not a plain running total;
//   * `max` that keeps the FIRST of equals, as Python's does, which decides
//     the "most common value" when two counts tie.
//
// No plotting library, for the same reason there is no pyserial in the
// Python: a line and some axes are not worth a dependency.
use crate::{clock, entropy, log};
use std::collections::BTreeMap;
use std::fs;
use std::hint::black_box;
use std::io;
use std::path::{Path, PathBuf};

pub const DEFAULT_TITLE: &str = "Radiation monitor";
pub const RANDOM_TITLE: &str = "Where the random comes from";
const VERSION: &str = env!("CARGO_PKG_VERSION");
const HOMEPAGE: &str = "https://github.com/vonglurt/radbeeper";
const TAGLINE: &str = "A GQ GMC Geiger-Muller counter on the desk";
const ENTROPY_BITS: i64 = entropy::ENTROPY_BITS as i64;

/// The screenshots the index links, when they are beside it.
const SHOTS: [&str; 9] = [
    "probe", "watch", "watch-filling", "watch-spectrum", "watch-plain",
    "log-output", "log-tabs", "commands", "watch-300-320",
];

// ------------------------------------------------------- python's numbers ---

/// `"%.Nf"`. Rust's fixed formatting rounds the exact binary value half to
/// even, which is what C's printf does and so what Python's `%` does.
fn f(places: usize, v: f64) -> String {
    format!("{:.*}", places, v)
}

/// `"{:,}".format(n)`.
fn commas(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    if n < 0 {
        format!("-{}", out)
    } else {
        out
    }
}

/// `"%.0e"`, which writes a two-digit signed exponent where Rust writes `e-4`.
fn e0(v: f64) -> String {
    let s = format!("{:.0e}", v);
    match s.split_once('e') {
        Some((m, e)) => {
            let exp: i32 = e.parse().unwrap_or(0);
            format!("{}e{}{:02}", m, if exp < 0 { '-' } else { '+' }, exp.abs())
        }
        None => s,
    }
}

/// Python's `max(a, b)`: `a` unless `b` is strictly greater.
fn pymax(a: f64, b: f64) -> f64 {
    if b > a {
        b
    } else {
        a
    }
}

/// C's `pow`, called the way Python's `**` calls it. black_box keeps LLVM from
/// turning `pow(x, 2.0)` into `x * x` or `pow(10.0, x)` into `exp10(x)`, which
/// are close to the same number and not always the same bits.
fn pow(x: f64, y: f64) -> f64 {
    black_box(x).powf(black_box(y))
}

/// `math.log(x, 2)`, which is log(x) / log(2) and not log2(x).
fn log_2(x: f64) -> f64 {
    x.ln() / 2.0f64.ln()
}

/// CPython's `sum()` over floats (3.12 onwards): Neumaier's compensated sum.
fn py_sum(values: impl Iterator<Item = f64>) -> f64 {
    let (mut hi, mut lo) = (0.0f64, 0.0f64);
    for x in values {
        let t = hi + x;
        if hi.abs() >= x.abs() {
            lo += (hi - t) + x;
        } else {
            lo += (x - t) + hi;
        }
        hi = t;
    }
    if lo != 0.0 && lo.is_finite() {
        hi + lo
    } else {
        hi
    }
}

fn factorial(k: u32) -> f64 {
    (1..=k as u64).product::<u64>() as f64
}

fn poisson_pmf(lam: f64, k: u32) -> f64 {
    (-lam).exp() * pow(lam, k as f64) / factorial(k)
}

/// Python's `int()` and `float()` on a cell: surrounding whitespace allowed.
fn int_cell(s: &str) -> Option<i64> {
    s.trim().parse().ok()
}

fn float_cell(s: &str) -> Option<f64> {
    s.trim().parse().ok()
}

// ------------------------------------------------------------------ paths ---
//
// os.path's lexical rules, because the links on the page are relative paths
// the Python worked out, and a `./` or a `..` that differs is a byte that
// differs.

/// `os.path.abspath`: joined to the working directory, then normalised
/// lexically -- no symlinks resolved.
fn abspath(p: &str) -> String {
    let joined = if p.starts_with('/') {
        p.to_string()
    } else {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        join(&cwd.to_string_lossy(), p)
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

/// `os.path.join(a, b)` for two parts.
fn join(a: &str, b: &str) -> String {
    if b.starts_with('/') || a.is_empty() {
        b.to_string()
    } else if a.ends_with('/') {
        format!("{}{}", a, b)
    } else {
        format!("{}/{}", a, b)
    }
}

fn dirname(p: &str) -> String {
    match p.rfind('/') {
        Some(i) => {
            let head = &p[..=i];
            let trimmed = head.trim_end_matches('/');
            if trimmed.is_empty() {
                head.to_string()
            } else {
                trimmed.to_string()
            }
        }
        None => String::new(),
    }
}

fn basename(p: &str) -> &str {
    match p.rfind('/') {
        Some(i) => &p[i + 1..],
        None => p,
    }
}

/// `os.path.relpath(path, start)`.
fn relpath(path: &str, start: &str) -> String {
    let s = abspath(start);
    let p = abspath(path);
    let sl: Vec<&str> = s.split('/').filter(|x| !x.is_empty()).collect();
    let pl: Vec<&str> = p.split('/').filter(|x| !x.is_empty()).collect();
    let common = sl.iter().zip(pl.iter()).take_while(|(a, b)| a == b).count();
    let mut rel: Vec<&str> = vec![".."; sl.len() - common];
    rel.extend(&pl[common..]);
    if rel.is_empty() {
        ".".to_string()
    } else {
        rel.join("/")
    }
}

fn path_str(p: &Path) -> String {
    p.to_string_lossy().into_owned()
}

// -------------------------------------------------------------- the clock ---

/// When the pages say they were generated.
///
/// SOURCE_DATE_EPOCH when it is set -- the reproducible-builds convention, and
/// the one hook the Python honours too -- so that two runs, or two
/// implementations, on the same logs write the same bytes. The wall clock
/// otherwise.
pub fn now() -> f64 {
    std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|v| v.trim().parse::<f64>().ok())
        .unwrap_or_else(clock::now)
}

// -------------------------------------------------------------- summarise ---

#[derive(Default)]
pub struct Day {
    pub rows: u64,
    pub counts: i64,
    pub seconds: f64,
    pub peak: Option<f64>,
}

#[derive(Default)]
pub struct Hour {
    pub counts: i64,
    pub seconds: f64,
    pub peak: Option<f64>,
}

/// One counter's worth of logs, as the page draws it.
#[derive(Default)]
pub struct Counter {
    pub serial: String,
    pub rows: u64,
    pub counts: i64,
    pub seconds: f64,
    pub first: Option<f64>,
    pub last: Option<f64>,
    pub peak: Option<f64>,
    pub days: BTreeMap<String, Day>,
    pub hours: BTreeMap<i64, Hour>,
    /// The last sixty rows, oldest first, as the raw cells of the file.
    pub latest: Vec<(f64, Vec<String>)>,
    pub cpm: f64,
    pub usvh: f64,
    /// (file name, link relative to the page) for every log of this counter.
    pub links: Vec<(String, String)>,
}

fn raise(slot: &mut Option<f64>, v: Option<f64>) {
    if let Some(p) = v {
        if slot.map(|s| p > s).unwrap_or(true) {
            *slot = Some(p);
        }
    }
}

/// (when, cells) for every row of a dated log, header and blanks skipped.
///
/// THE CELLS ARE POSITIONAL, as the Python's `read_rows` hands them to the
/// page: counts at 2, seconds at 3, the first peak at 7, site at 11. That is
/// not `log::read_table`'s match-by-name, and it is not meant to be -- the
/// page is held to the Python's bytes, and the Python reads by position.
fn read_rows(path: &Path) -> Vec<(f64, Vec<String>)> {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return Vec::new(),
    };
    text.lines()
        .filter_map(|line| {
            let when = log::row_time(line)?;
            Some((when, line.split('\t').map(str::to_string).collect()))
        })
        .collect()
}

/// Per counter: the totals, the daily rollup, and the last rows seen.
///
/// The daily rollup is counts over seconds, not a mean of the per-row CPMs:
/// rows are not all the same length -- a service stopped mid-interval writes
/// a short one -- and averaging averages would weight a four-second row like
/// a thirty-second one.
pub fn summarise(directory: &Path, cpm_per_usvh: f64) -> BTreeMap<String, Counter> {
    let mut counters: BTreeMap<String, Counter> = BTreeMap::new();
    for (serial, path) in log::files(directory) {
        let c = counters.entry(serial.clone()).or_insert_with(|| Counter {
            serial: serial.clone(),
            ..Counter::default()
        });
        for (when, cells) in read_rows(&path) {
            let (counts, seconds) = match (
                cells.get(2).and_then(|s| int_cell(s)),
                cells.get(3).and_then(|s| float_cell(s)),
            ) {
                (Some(n), Some(s)) => (n, s),
                _ => continue,
            };
            let peak = cells.get(7).filter(|s| !s.is_empty()).and_then(|s| float_cell(s));
            c.rows += 1;
            c.counts += counts;
            c.seconds += seconds;
            c.first = Some(c.first.map_or(when, |f| if when < f { when } else { f }));
            c.last = Some(c.last.map_or(when, |l| if when > l { when } else { l }));
            raise(&mut c.peak, peak);
            // An hour is the plot's resolution: eleven days of thirty-second
            // rows is 28,000 points, which is more ink than the page is wide.
            let h = c.hours.entry((when / 3600.0).floor() as i64).or_default();
            h.counts += counts;
            h.seconds += seconds;
            raise(&mut h.peak, peak);
            let d = c.days.entry(clock::format(when, "%Y-%m-%d")).or_default();
            d.rows += 1;
            d.counts += counts;
            d.seconds += seconds;
            raise(&mut d.peak, peak);
            c.latest.push((when, cells));
        }
    }
    for c in counters.values_mut() {
        c.latest.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
        // Sixty rows is half an hour and about four screens. The rest of the
        // record is one link away as the file it already lives in.
        let keep = c.latest.len().saturating_sub(60);
        c.latest.drain(..keep);
        c.cpm = if c.seconds != 0.0 { c.counts as f64 * 60.0 / c.seconds } else { 0.0 };
        c.usvh = if cpm_per_usvh != 0.0 { c.cpm / cpm_per_usvh } else { 0.0 };
    }
    counters
}

/// A round number at or above value, for an axis somebody can read.
pub fn nice_ceiling(value: f64) -> f64 {
    if value <= 0.0 {
        return 1.0;
    }
    let power = pow(10.0, value.log10().floor());
    for step in [1.0, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0] {
        if value <= step * power {
            return step * power;
        }
    }
    10.0 * power
}

/// [(unix time, mean CPM, peak CPM or None)] per hour, in order.
pub fn hour_series(c: &Counter) -> Vec<(f64, f64, Option<f64>)> {
    c.hours
        .iter()
        .filter(|(_, h)| h.seconds != 0.0)
        .map(|(hour, h)| (*hour as f64 * 3600.0, h.counts as f64 * 60.0 / h.seconds, h.peak))
        .collect()
}

// ------------------------------------------------------------- the charts ---

/// An inline SVG line chart of CPM by the hour, on a log axis.
///
/// THE LINE BREAKS OVER A GAP rather than running across it: an hour the
/// counter was not recording is not an hour of zero. THE AXIS IS LOGARITHMIC
/// because the data is -- background near 40 CPM, peaks two hundred times
/// that.
pub fn svg_plot(series: &[(f64, f64, Option<f64>)]) -> String {
    let (width, height, pad_l, pad_b, pad_t, pad_r) = (1120i64, 1200i64, 52i64, 26i64, 12i64, 10i64);
    if series.len() < 2 {
        return String::new();
    }
    let mut values: Vec<f64> = series.iter().map(|s| s.1).filter(|m| *m > 0.0).collect();
    values.extend(series.iter().filter_map(|s| s.2).filter(|p| *p != 0.0));
    if values.is_empty() {
        return String::new();
    }
    // min() and max() as Python's: the first of equals.
    let vmin = values.iter().copied().fold(values[0], |a, b| if b < a { b } else { a });
    let vmax = values.iter().copied().fold(values[0], |a, b| if b > a { b } else { a });
    let lo = pow(10.0, vmin.log10().floor());
    let mut hi = pow(10.0, vmax.log10().ceil());
    if hi <= lo {
        hi = lo * 10.0;
    }
    let span_y = hi.log10() - lo.log10();
    let t0 = series[0].0;
    let t1 = series[series.len() - 1].0;
    let span = if t1 - t0 != 0.0 { t1 - t0 } else { 1.0 };
    let iw = width - pad_l - pad_r;
    let ih = height - pad_t - pad_b;
    let x_of = |t: f64| pad_l as f64 + iw as f64 * (t - t0) / span;
    let y_of = |v: f64| {
        pad_t as f64 + ih as f64 * (1.0 - (pymax(v, lo).log10() - lo.log10()) / span_y)
    };
    // An hour and a half between points means an hour went unrecorded.
    let step = 3600.0 * 1.5;
    let path = |points: &[(f64, Option<f64>)]| -> String {
        let mut d: Vec<String> = Vec::new();
        let mut pen = false;
        let mut last: Option<f64> = None;
        for (t, v) in points {
            let v = match v {
                Some(v) => *v,
                None => {
                    pen = false;
                    last = Some(*t);
                    continue;
                }
            };
            let letter = if !pen || last.map(|l| t - l > step).unwrap_or(false) { "M" } else { "L" };
            d.push(format!("{}{} {}", letter, f(1, x_of(*t)), f(1, y_of(v))));
            pen = true;
            last = Some(*t);
        }
        d.join(" ")
    };

    let mut out = vec![format!(
        "<svg class=\"plot\" viewBox=\"0 0 {} {}\" width=\"100%\" \
         preserveAspectRatio=\"xMidYMid meet\" role=\"img\" \
         aria-label=\"counts per minute over time\">",
        width, height
    )];
    // y grid: one line per decade, with the halfway marks left unlabelled.
    let mut decade = lo;
    while decade <= hi * 1.0001 {
        let y = f(1, y_of(decade));
        out.push(format!(
            "<line class=\"grid\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/>",
            pad_l, y, width - pad_r, y
        ));
        out.push(format!(
            "<text class=\"ylab\" x=\"{}\" y=\"{}\">{}</text>",
            pad_l - 8,
            f(1, y_of(decade) + 4.0),
            commas(decade as i64)
        ));
        for minor in [2.0, 5.0] {
            let v = decade * minor;
            if v < hi {
                let ym = f(1, y_of(v));
                out.push(format!(
                    "<line class=\"grid minor\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/>",
                    pad_l, ym, width - pad_r, ym
                ));
            }
        }
        decade *= 10.0;
    }
    // x ticks at midnight
    let day = 86400.0;
    let mut t = t0 - t0.rem_euclid(day) + day;
    while t < t1 {
        let x = f(1, x_of(t));
        out.push(format!(
            "<line class=\"grid\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/>",
            x, pad_t, x, pad_t + ih
        ));
        out.push(format!(
            "<text class=\"xlab\" x=\"{}\" y=\"{}\">{}</text>",
            x,
            height - 8,
            clock::format(t, "%b %-d")
        ));
        t += day;
    }
    let peaks: Vec<(f64, Option<f64>)> = series.iter().map(|s| (s.0, s.2)).collect();
    let means: Vec<(f64, Option<f64>)> = series.iter().map(|s| (s.0, Some(s.1))).collect();
    out.push(format!("<path class=\"peak\" d=\"{}\"/>", path(&peaks)));
    out.push(format!("<path class=\"mean\" d=\"{}\"/>", path(&means)));
    out.push(format!(
        "<text class=\"ycap\" x=\"{}\" y=\"{}\">CPM, log</text>",
        pad_l - 44,
        pad_t + 10
    ));
    out.push("</svg>".to_string());
    out.concat()
}

pub fn esc(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// The <head> and the one stylesheet both generated pages use.
///
/// Two pages that look like two different projects is how a reader stops
/// believing they came from the same program, so the rules live here once
/// and random.html adds only what is peculiar to it.
pub fn page_head(title: &str, extra: &[&str]) -> Vec<String> {
    let mut out: Vec<String> = [
        "<!doctype html>",
        "<html lang=\"en\"><head><meta charset=\"utf-8\">",
        "<meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    out.push(format!("<title>{}</title>", esc(title)));
    out.push("<style>".to_string());
    for rule in STYLE.iter().chain(extra.iter()) {
        out.push(rule.to_string());
    }
    out.push("</style></head><body><main>".to_string());
    out
}

const STYLE: &[&str] = &[
    ":root{color-scheme:light dark;--fg:#181818;--bg:#faf9f7;\
     --dim:#6b6b6b;--line:#e0ddd8;--card:#fff;--accent:#2f6f4f;\
     --warn:#b0642a}",
    "@media (prefers-color-scheme:dark){:root{--fg:#e8e6e3;--bg:#16161a;\
     --dim:#9a9a9a;--line:#2e2e35;--card:#1e1e24;--accent:#6cc08c;\
     --warn:#e0a06a}}",
    "body{margin:0;padding:2rem 1rem;background:var(--bg);color:var(--fg);\
     font:15px/1.55 system-ui,-apple-system,Segoe UI,sans-serif}",
    "main{max-width:1100px;margin:0 auto}",
    "h1{font-size:1.6rem;margin:0 0 .2rem}",
    "h2{font-size:1.1rem;margin:2.2rem 0 .6rem;font-weight:600}",
    ".sub{color:var(--dim);margin:0 0 1rem}",
    ".lede{max-width:70ch;margin:0 0 2rem;font-size:1rem;line-height:1.6}",
    ".lede a{color:var(--accent)}",
    ".how{margin-top:2.6rem}",
    ".how h3{margin:1.6rem 0 .3rem;font-size:1rem}",
    ".how p{margin:.35rem 0;max-width:68ch}",
    ".how pre{background:var(--card);border:1px solid var(--line);\
     border-radius:8px;padding:.6rem .9rem;overflow-x:auto;\
     font-size:.84rem;margin:.5rem 0}",
    ".how img{border:1px solid var(--line);border-radius:8px;\
     margin:.6rem 0;max-width:100%}",
    ".how figure{margin:.8rem 0 1.2rem}",
    ".how figcaption{color:var(--dim);font-size:.82rem;margin-top:.3rem}",
    ".cards{display:grid;gap:.9rem;\
     grid-template-columns:repeat(auto-fit,minmax(190px,1fr))}",
    ".card{background:var(--card);border:1px solid var(--line);\
     border-radius:10px;padding:.9rem 1rem}",
    ".card .k{color:var(--dim);font-size:.78rem;text-transform:uppercase;\
     letter-spacing:.04em}",
    ".card .v{font-size:1.5rem;font-variant-numeric:tabular-nums;\
     margin-top:.15rem}",
    ".card .n{color:var(--dim);font-size:.8rem}",
    ".plotwrap{background:var(--card);border:1px solid var(--line);\
     border-radius:10px;padding:.9rem 1rem 0;margin-top:.9rem}",
    ".plot{display:block;height:auto}",
    ".plot .grid{stroke:var(--line);stroke-width:1}",
    ".plot .minor{opacity:.45}",
    ".plot .mean{fill:none;stroke:var(--accent);stroke-width:1.6;\
     stroke-linejoin:round;stroke-linecap:round}",
    ".plot .peak{fill:none;stroke:var(--warn);stroke-width:1;opacity:.55}",
    ".plot text{fill:var(--dim);font-size:11px;\
     font-family:system-ui,sans-serif}",
    ".plot .ylab{text-anchor:end}",
    ".plot .xlab{text-anchor:middle}",
    ".plot .ycap{text-anchor:start;font-size:10px;letter-spacing:.05em;\
     text-transform:uppercase}",
    ".legend{display:flex;gap:1.2rem;color:var(--dim);font-size:.8rem;\
     padding:.1rem 0 .8rem}",
    ".legend i{display:inline-block;width:18px;height:0;\
     border-top-width:2px;border-top-style:solid;vertical-align:middle;\
     margin-right:.4rem}",
    ".about{background:var(--card);border:1px solid var(--line);\
     border-radius:10px;padding:1.2rem 1.4rem;margin-top:2.6rem}",
    ".about h3{margin:0 0 .4rem;font-size:1.05rem}",
    ".about p{margin:.4rem 0;max-width:62ch}",
    ".about pre{background:var(--bg);border:1px solid var(--line);\
     border-radius:6px;padding:.6rem .8rem;overflow-x:auto;font-size:.82rem}",
    ".windows{display:grid;gap:.7rem;margin:.9rem 0 0;padding:0;\
     list-style:none;grid-template-columns:repeat(auto-fit,minmax(190px,1fr))}",
    ".windows li{border-left:2px solid var(--accent);padding-left:.7rem}",
    ".windows b{display:block;font-size:1.05rem}",
    ".windows span{color:var(--dim);font-size:.85rem}",
    "table{border-collapse:collapse;width:100%;font-size:.86rem;\
     font-variant-numeric:tabular-nums;background:var(--card);\
     border:1px solid var(--line);border-radius:10px;overflow:hidden}",
    "th{text-align:left;font-weight:600;color:var(--dim);font-size:.76rem;\
     text-transform:uppercase;letter-spacing:.04em}",
    "th,td{padding:.36rem .7rem;border-bottom:1px solid var(--line)}",
    "tbody tr:last-child td{border-bottom:0}",
    "td:not(:first-child),th:not(:first-child){text-align:right}",
    ".more{margin:.6rem 0 0;font-size:.85rem}",
    ".more a{color:var(--accent)}",
    ".tablewrap{overflow-x:auto}",
    "footer{color:var(--dim);font-size:.82rem;margin-top:3rem;\
     border-top:1px solid var(--line);padding-top:1rem}",
    "code{background:var(--card);padding:.1rem .3rem;border-radius:4px}",
];

// -------------------------------------------------------------- index.html ---

/// One self-contained page: no scripts, no fetches, no web fonts.
pub fn render_html(
    counters: &BTreeMap<String, Counter>,
    sites: &[(String, f64, String)],
    cpm_per_usvh: f64,
    title: &str,
    shots: &BTreeMap<String, String>,
    random_link: Option<&str>,
) -> String {
    let when = |t: f64| clock::format(t, "%Y-%m-%d %H:%M");
    let mut out = page_head(title, &[]);
    macro_rules! a {
        ($($arg:tt)*) => { out.push(format!($($arg)*)) };
    }
    a!("<h1>{}</h1>", esc(title));
    a!(
        "<p class=\"sub\">{} &middot; generated {} by radbeeper {} &middot; tube factor {} CPM per &micro;Sv/h</p>",
        esc(TAGLINE),
        esc(&when(now())),
        VERSION,
        f(1, cpm_per_usvh)
    );
    a!(
        "<p class=\"lede\">A <strong>GQ GMC-320 Plus</strong> Geiger&ndash;M&uuml;ller \
         counter, plugged into a machine running <strong>Alpine Linux</strong> \
         &mdash; or <strong><a href=\"https://github.com/vonglurt/copal\">Copal\
         </a></strong>, its distillation, which carries this as a stage and \
         installs the <code>linux-lts</code> kernel package the counter needs to \
         be seen at all &mdash; as a USB serial device, here shared into a \
         virtual machine over USB pass-through. RadBeeper reads the counter, logs it to tab-separated \
         files, and <strong>this page is regenerated from those files by a \
         GitHub Action</strong> on every push. The whole of it is forkable: copy \
         your own logs into <code>logs/</code> and you get a page like this \
         one. <a href=\"{}\">Source and instructions</a>.{}</p>",
        HOMEPAGE,
        match random_link {
            Some(l) => format!(" <a href=\"{}\">Where the random comes from</a>.", esc(l)),
            None => String::new(),
        }
    );

    if counters.is_empty() {
        a!("<p>No logs found. Put <code>cpm-&lt;serial&gt;-YYYY-MM.tsv</code> \
            files beside this page and run <code>radbeeper export</code>.</p>");
    }
    for (serial, c) in counters {
        let at = match c.last {
            Some(l) if l != 0.0 => l,
            _ => now(),
        };
        let here = log::site_at(serial, at, sites);
        a!("<h2>Counter {}</h2>", esc(serial));
        a!("<div class=\"cards\">");
        let card = |k: &str, v: &str, n: &str| {
            format!(
                "<div class=\"card\"><div class=\"k\">{}</div><div class=\"v\">{}</div>{}</div>",
                esc(k),
                esc(v),
                if n.is_empty() { String::new() } else { format!("<div class=\"n\">{}</div>", esc(n)) }
            )
        };
        out.push(card("Mean", &format!("{} CPM", f(1, c.cpm)), &format!("{} uSv/h", f(3, c.usvh))));
        out.push(match c.peak {
            None => card("Highest 3s peak", "--", ""),
            Some(p) => card(
                "Highest 3s peak",
                &format!("{} CPM", f(0, p)),
                &format!("{} uSv/h", f(3, p / cpm_per_usvh)),
            ),
        });
        out.push(card(
            "Rows",
            &commas(c.rows as i64),
            &format!("{} hours counted", f(1, c.seconds / 3600.0)),
        ));
        let stamp = |t: Option<f64>| match t {
            Some(t) if t != 0.0 => when(t),
            _ => "--".to_string(),
        };
        out.push(card("Covering", &stamp(c.first), &format!("to {}", stamp(c.last))));
        if let Some(name) = here {
            out.push(card("Site", &name, ""));
        }
        a!("</div>");

        let chart = svg_plot(&hour_series(c));
        if !chart.is_empty() {
            a!("<h2>Counts per minute, by the hour</h2>");
            a!("<div class=\"plotwrap\">{}", chart);
            a!("<div class=\"legend\">\
                <span><i style=\"border-top-color:var(--accent)\"></i>\
                hourly mean</span>\
                <span><i style=\"border-top-color:var(--warn)\"></i>\
                highest 3-second peak in the hour</span>\
                <span>a break in the line is an hour with no recording</span>\
                </div></div>");
        }

        a!("<h2>By day</h2>");
        a!("<div class=\"tablewrap\"><table><thead><tr><th>Day</th>\
            <th>Mean CPM</th>\
            <th>&micro;Sv/h</th><th>Peak 3s CPM</th><th>Rows</th>\
            <th>Hours</th></tr></thead><tbody>");
        for (day, d) in c.days.iter().rev() {
            let cpm = if d.seconds != 0.0 { d.counts as f64 * 60.0 / d.seconds } else { 0.0 };
            a!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(day),
                f(1, cpm),
                f(3, cpm / cpm_per_usvh),
                d.peak.map(|p| f(0, p)).unwrap_or_else(|| "--".to_string()),
                d.rows,
                f(1, d.seconds / 3600.0)
            );
        }
        a!("</tbody></table></div>");

        a!("<h2>Latest rows</h2>");
        a!("<div class=\"tablewrap\"><table><thead><tr><th>Time</th>\
            <th>CPS</th><th>3s</th><th>30s</th><th>300s</th><th>Peak 3s</th>\
            <th>Source</th><th>Site</th></tr></thead><tbody>");
        for (_t, cells) in c.latest.iter().rev() {
            let cell = |i: usize| match cells.get(i) {
                Some(s) if !s.is_empty() => esc(s),
                _ => "--".to_string(),
            };
            a!(
                "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
                esc(&cells[0]),
                cell(1),
                cell(4),
                cell(5),
                cell(6),
                cell(7),
                cell(10),
                cell(11)
            );
        }
        a!("</tbody></table></div>");
        if !c.links.is_empty() {
            let links: Vec<String> = c
                .links
                .iter()
                .map(|(name, href)| format!("<a href=\"{}\">{}</a>", href, esc(name)))
                .collect();
            a!(
                "<p class=\"more\">The last {} rows of {}. Read the full chart: {} &mdash; tab-separated, one row every 30 seconds.</p>",
                c.latest.len(),
                commas(c.rows as i64),
                links.join(" &middot; ")
            );
        }
    }

    // THE HOW-TO AND THE DATA ON ONE PAGE. Somebody arriving from a link
    // wants to know what they are looking at and how to get their own.
    a!("<section class=\"how\">");
    a!("<h2>How to use it</h2>");
    let shot = |out: &mut Vec<String>, name: &str, caption: &str| {
        if let Some(src) = shots.get(name) {
            out.push(format!(
                "<figure><img src=\"{}\" alt=\"{}\" loading=\"lazy\"><figcaption>{}</figcaption></figure>",
                esc(src),
                esc(caption),
                esc(caption)
            ));
        }
    };

    a!("<h3>What you need</h3>");
    a!("<p><b>A GQ GMC-320 Plus, plugged into a USB port, switched on.</b> \
        That is the hardware, and no amount of software substitutes for it: \
        every number on this page came off a real tube over a real serial \
        port. A <b>GMC-300</b> works too &mdash; RadBeeper tries its 57600 \
        baud as well as the 320's 115200 &mdash; and a 500 or 600 is found \
        and read, but its tube is not an M4011, so it needs its own \
        <code>--cpm-per-usvh</code>.</p>");
    a!("<ul>");
    a!("<li><b>The USB cable that came with it.</b> A charge-only cable \
        carries no data, and is indistinguishable from an empty port until \
        you try another one.</li>");
    a!("<li><b>The counter switched on.</b> Its USB-serial chip is powered \
        by the counter, not by the bus &mdash; a flat 320 enumerates as \
        nothing.</li>");
    a!("<li><b>A kernel with <code>ch341</code>.</b> The 320 Plus presents \
        as a CH340. <code>linux-lts</code> and <code>linux-rpi</code> carry \
        the driver; Alpine's <code>linux-virt</code> does not, which is the \
        commonest reason a counter that is plugged in cannot be found. \
        <a href=\"https://github.com/vonglurt/copal\">Copal</a> installs the \
        <code>linux-lts</code> Alpine package, so the case does not arise \
        there.</li>");
    a!("<li><b>Membership of <code>dialout</code>.</b> The serial node is \
        <code>root:dialout</code> and RadBeeper does not want root.</li>");
    a!("</ul>");
    a!("<p>Plugged in and working, Linux says so: <code>dmesg</code> ends \
        with <code>ch341-uart converter now attached to ttyUSB0</code> and \
        <code>/dev/ttyUSB0</code> exists. USB pass-through into a VM counts \
        &mdash; these logs came from one &mdash; provided the guest kernel \
        is one of the two that has the driver.</p>");
    a!("<p>No counter yet? <code>radbeeper --source sim watch</code> draws \
        the whole monitor against a synthetic Poisson background.</p>");

    a!("<h3>Install</h3>");
    a!("<p>One stdlib Python file. No packages, no build, nothing to compile \
        &mdash; the install is a copy.</p>");
    a!("<pre>git clone {}.git\n\
        cd radbeeper &amp;&amp; make install\n\n\
        doas adduser $USER dialout    # then log in again</pre>", HOMEPAGE);
    a!("<p>The read side is also a Rust crate, if a native binary suits the \
        machine better: <code>cargo install radbeeper</code>, or a static \
        musl build from the <a href=\"{}/releases\">releases \
        page</a> for x86_64, aarch64, armv7 or the Pi Zero's armv6. It does \
        <code>probe</code>, <code>cpm</code> and <code>watch</code>; \
        everything that writes these logs stays in the Python.</p>", HOMEPAGE);

    a!("<h3>Find the counter</h3>");
    a!("<pre>radbeeper probe</pre>");
    shot(&mut out, "probe", "radbeeper probe, against the counter these logs came from");
    a!("<p>If it finds nothing it says which of four things went wrong, because \
        they have four different fixes: no serial node at all (often a kernel \
        with no <code>ch341</code> &mdash; Alpine's <code>linux-virt</code> has \
        none, <code>linux-lts</code> does), permission on the node, something \
        that is not a GMC, or a port another reader already holds.</p>");

    a!("<h3>Watch it</h3>");
    a!("<pre>radbeeper watch</pre>");
    shot(&mut out, "watch", "the monitor: five time constants, the counts, the \
        spectrum and a line of decay-derived random");
    shot(&mut out, "watch-300-320", "seconds 300 to 320 of a real session, at ten times \
        speed");
    a!("<p>The counter's own reading is a rolling 60-second count &mdash; one \
        number with one time constant. RadBeeper counts the blips itself and \
        keeps five windows at once, each a factor of ten apart.</p>");
    a!("<ul class=\"windows\">");
    a!("<li><b>3 s</b><span>Three seconds is too short to be a measurement. \
        On a 40 CPM background it swings between 0 and 80, because at this \
        rate three seconds is two counts. Watch it to see a source come and \
        go as you move it, and do not write it down.</span></li>");
    a!("<li><b>30 s</b><span>Half a minute, and the shortest window that is a \
        count rather than a flicker. Settled enough to compare two places, \
        quick enough to follow your hands &mdash; which is why it is the \
        number in the big digits, and what <code>radbeeper cpm</code> \
        reports.</span></li>");
    a!("<li><b>300 s</b><span>Five minutes. A number worth writing \
        down.</span></li>");
    a!("<li><b>3000 s</b><span>Fifty minutes. What the background here \
        actually is, once the day's traffic through the room has averaged \
        out.</span></li>");
    a!("<li><b>30000 s</b><span>Eight hours twenty &mdash; a working day. It \
        is not a faster answer to the same question, it is the only window \
        that spans one; a shift's worth of background, against which a day \
        that was different is visible as a difference.</span></li>");
    a!("</ul>");
    a!("<p><strong>A window shows nothing until it is full</strong>, and says \
        how long it still needs. A three-second CPM built from one sample is \
        twenty times noisier than it looks, and drawing it as though it were \
        settled is how a 25 CPM background reads as 60 and somebody goes \
        hunting for a leak.</p>");
    shot(&mut out, "watch-filling", "the long window still filling, and saying so");
    a!("<h3>Is anything periodic?</h3>");
    a!("<p>The monitor also accumulates a power spectrum of the per-second \
        counts. <strong>Decay is a Poisson process and the spectrum of a \
        Poisson process is flat</strong> &mdash; so a featureless strip is the \
        good answer, and says that nothing is arriving on a schedule.</p>");
    shot(&mut out, "watch-spectrum", "the accumulating spectrum: flat is healthy");
    a!("<p>A peak is the interesting case: mains hum on the tube's supply, a \
        fan carrying a source past, a loose connector, firmware that batches \
        its reporting. In the time domain all of those look exactly like more \
        counts. Averaging successive windows is what makes a real line climb \
        out of the noise &mdash; one periodogram of a Poisson process is flat \
        in expectation and violently noisy in fact.</p>");
    a!("<p>No counter on the desk? Every command runs against a built-in \
        source, and it is a real one &mdash; radioactive decay is a Poisson \
        process, so the simulator draws Poisson samples.</p>");
    a!("<pre>radbeeper --source sim --sim-cpm 400 watch</pre>");

    a!("<h3>Random numbers out of decay</h3>");
    a!("<p>The monitor also keeps an entropy pool and prints a line of hex \
        whenever the samples have <strong>earned</strong> it. Not modelled \
        &mdash; measured: the counts coming back over the serial link are \
        measurably burstier than Poisson, so the bits per second are \
        estimated from the samples themselves and pushed to the far end of a \
        99% confidence interval before anything is emitted.</p>");
    a!("<pre>radbeeper random</pre>");
    if let Some(l) = random_link {
        a!("<p><a href=\"{}\"><strong>Where the random \
            comes from</strong></a> &mdash; the counts against the model, the \
            bits accumulating second by second, what the serial link's \
            one-second resolution costs, and every line emitted with the \
            counts behind it.</p>", esc(l));
    }

    a!("<h3>Log it</h3>");
    a!("<pre>radbeeper service</pre>");
    a!("<p>A row every 30 seconds into a dated file per counter, \
        <code>cpm-&lt;serial&gt;-YYYY-MM.tsv</code> &mdash; which is rotation \
        by construction, with no cron entry and nothing renaming a file while \
        a service appends to it. Each row carries the <strong>peak</strong> \
        every window reached since the last one, so a source that came and \
        went between two rows still leaves a trace.</p>");
    shot(&mut out, "log-output", "the log on disk, and what a row holds");
    a!("<p>Tabs are invisible, and that matters here: an <strong>empty field is \
        not a zero</strong>, it is a window that was not full yet.</p>");
    shot(&mut out, "log-tabs", "the same rows with every tab made visible");
    a!("<p>The counter also records to its own flash whether or not anything \
        is listening. <code>radbeeper backfill</code> reads that back and \
        fills the gaps &mdash; one row per slot, never over a live \
        measurement, and an hour nobody recorded stays an hour nobody \
        recorded.</p>");
    a!("<pre>radbeeper backfill</pre>");

    a!("<h3>Publish it</h3>");
    a!("<pre>radbeeper export --logs logs -o index.html</pre>");
    a!("<p>This page. Fork the repository, copy your <code>cpm-*.tsv</code> \
        into <code>logs/</code>, push, and the workflow rebuilds and commits \
        it &mdash; served by GitHub Pages with no build step. There is nothing \
        to install in the workflow: the generator is the same one file, which \
        is also why the page cannot drift from the log format.</p>");
    a!(
        "<p>MIT License &mdash; Copyright (c) 2026 Paul Richeson &middot; <a href=\"{}\">{}</a></p>",
        HOMEPAGE,
        HOMEPAGE.split_once("//").map(|(_, rest)| rest).unwrap_or(HOMEPAGE)
    );
    a!("</section>");
    a!("<footer>MIT License &mdash; Copyright (c) 2026 Paul Richeson. \
        Rows marked <code>live</code> were measured by a monitor on \
        this machine; rows marked <code>flash</code> were reconstructed from \
        the counter's own recorded history. Gaps are gaps: nothing is \
        interpolated. Built by \
        <code>radbeeper export</code>.</footer>");
    a!("</main>");
    a!("</body></html>");
    out.join("\n") + "\n"
}

// ------------------------------------------------------------- random.html ---

/// [(serial, path)] for every emission log in a directory.
pub fn random_files(directory: &Path) -> Vec<(String, PathBuf)> {
    let mut names: Vec<String> = match fs::read_dir(directory) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .collect(),
        Err(_) => return Vec::new(),
    };
    names.sort();
    names
        .into_iter()
        .filter(|n| n.starts_with("random-") && n.ends_with(".tsv") && n.len() >= 11)
        .map(|n| (n[7..n.len() - 4].to_string(), directory.join(&n)))
        .collect()
}

/// Per-second counts, tallied in the order each value first appeared -- the
/// order a Python `Counter` iterates in, which decides ties.
pub struct Freq {
    order: Vec<u32>,
    counts: BTreeMap<u32, usize>,
}

impl Freq {
    fn of(samples: &[u32]) -> Freq {
        let mut f = Freq { order: Vec::new(), counts: BTreeMap::new() };
        for s in samples {
            let n = f.counts.entry(*s).or_insert(0);
            if *n == 0 {
                f.order.push(*s);
            }
            *n += 1;
        }
        f
    }

    fn get(&self, k: u32) -> usize {
        self.counts.get(&k).copied().unwrap_or(0)
    }

    /// The most common value; the first seen of equals.
    fn mode(&self) -> u32 {
        let mut best = self.order[0];
        for k in &self.order {
            if self.get(*k) > self.get(best) {
                best = *k;
            }
        }
        best
    }
}

/// Everything the page says about a set of per-second counts: measured
/// first, modelled second, and the ratio between them.
pub struct Budget {
    pub n: usize,
    pub mean: f64,
    pub var: f64,
    pub dispersion: f64,
    pub pmax: f64,
    pub measured: f64,
    pub modelled: f64,
    pub ratio: f64,
    pub freq: Freq,
    pub seconds_for: Option<f64>,
    pub seconds_modelled: Option<f64>,
}

pub fn entropy_budget(samples: &[u32]) -> Option<Budget> {
    let n = samples.len();
    if n < 2 {
        return None;
    }
    let freq = Freq::of(samples);
    let total: u64 = samples.iter().map(|x| *x as u64).sum();
    let mean = total as f64 / n as f64;
    let var = py_sum(samples.iter().map(|x| pow(*x as f64 - mean, 2.0))) / (n - 1) as f64;
    let most = freq.get(freq.mode());
    let measured = entropy::mcv_min_entropy(samples);
    let modelled = entropy::poisson_min_entropy(mean);
    let bits = entropy::ENTROPY_BITS;
    Some(Budget {
        n,
        mean,
        var,
        dispersion: if mean != 0.0 { var / mean } else { 0.0 },
        pmax: most as f64 / n as f64,
        measured,
        modelled,
        ratio: if measured != 0.0 { modelled / measured } else { 0.0 },
        freq,
        seconds_for: (measured != 0.0).then(|| bits / measured),
        seconds_modelled: (modelled != 0.0).then(|| bits / modelled),
    })
}

/// Observed counts against the Poisson the model assumed, side by side, on a
/// log axis -- the interesting disagreement is in the tail.
pub fn svg_hist(freq: &Freq, lam: f64) -> String {
    let (width, height, pad_l, pad_b, pad_t, pad_r) = (1120i64, 340i64, 54i64, 34i64, 14i64, 12i64);
    let n: usize = freq.counts.values().sum();
    if n == 0 {
        return String::new();
    }
    let top = freq.counts.keys().next_back().copied().unwrap_or(0);
    let kmax = top.max(8).min(12);
    let lo = 1e-5f64;
    let hi = 1.0f64;
    let span = hi.log10() - lo.log10();
    let iw = width - pad_l - pad_r;
    let ih = height - pad_t - pad_b;
    let slot = iw as f64 / (kmax + 1) as f64;
    let bw = slot * 0.36;
    let y_of = |p: f64| pad_t as f64 + ih as f64 * (1.0 - (pymax(p, lo).log10() - lo.log10()) / span);

    let mut out = vec![format!(
        "<svg class=\"plot hist\" viewBox=\"0 0 {} {}\" width=\"100%\" \
         preserveAspectRatio=\"xMidYMid meet\" role=\"img\" aria-label=\"\
         observed counts per second against the Poisson model\">",
        width, height
    )];
    for decade in [1.0, 0.1, 0.01, 0.001, 0.0001, 0.00001] {
        let y = y_of(decade);
        out.push(format!(
            "<line class=\"grid\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/>",
            pad_l, f(1, y), width - pad_r, f(1, y)
        ));
        out.push(format!(
            "<text class=\"ylab\" x=\"{}\" y=\"{}\">{}</text>",
            pad_l - 8,
            f(1, y + 4.0),
            if decade >= 0.001 { log::g(decade) } else { e0(decade) }
        ));
    }
    let base = y_of(lo);
    for k in 0..=kmax {
        let x = pad_l as f64 + slot * k as f64 + slot / 2.0;
        let bars = [
            (poisson_pmf(lam, k), "modelled"),
            (freq.get(k) as f64 / n as f64, "seen"),
        ];
        for (i, (p, cls)) in bars.iter().enumerate() {
            if *p <= 0.0 {
                continue;
            }
            let y = y_of(*p);
            out.push(format!(
                "<rect class=\"{}\" x=\"{}\" y=\"{}\" width=\"{}\" height=\"{}\"/>",
                cls,
                f(1, x + (i as i64 - 1) as f64 * bw),
                f(1, y),
                f(1, bw),
                f(1, pymax(0.0, base - y))
            ));
        }
        out.push(format!(
            "<text class=\"xlab\" x=\"{}\" y=\"{}\">{}</text>",
            f(1, x),
            height - 12,
            k
        ));
    }
    out.push(format!(
        "<text class=\"xcap\" x=\"{}\" y=\"{}\">counts in one second</text>",
        pad_l,
        height - 1
    ));
    out.push(format!(
        "<text class=\"ycap\" x=\"{}\" y=\"{}\">share of seconds</text>",
        pad_l - 46,
        pad_t - 2
    ));
    out.push("</svg>".to_string());
    out.concat()
}

/// Bits accumulating against seconds: measured, and what was claimed. Where
/// the measured line crosses the target is when a line is emitted.
pub fn svg_bits(samples: &[u32]) -> String {
    let want = ENTROPY_BITS;
    let (width, height, pad_l, pad_b, pad_t, pad_r) = (1120i64, 320i64, 54i64, 34i64, 14i64, 12i64);
    let n = samples.len();
    if n < 30 {
        return String::new();
    }
    let step = (n / 240).max(1);
    let mut xs: Vec<usize> = Vec::new();
    let mut meas: Vec<f64> = Vec::new();
    let mut model: Vec<f64> = Vec::new();
    let mut pool = entropy::Entropy::new(want as f64);
    for (i, x) in samples.iter().enumerate() {
        let i = i + 1;
        pool.add(*x);
        if i % step != 0 && i != n {
            continue;
        }
        xs.push(i);
        meas.push(pool.bits());
        model.push(entropy::poisson_min_entropy(pool.rate()) * i as f64);
    }
    let top = model.iter().copied().fold(model[0], |a, b| if b > a { b } else { a });
    let hi = pymax(top, want as f64) * 1.05;
    let iw = width - pad_l - pad_r;
    let ih = height - pad_t - pad_b;
    let x_of = |t: usize| pad_l as f64 + (iw * t as i64) as f64 / n as f64;
    let y_of = |v: f64| pad_t as f64 + ih as f64 * (1.0 - v / hi);
    let path = |ys: &[f64]| -> String {
        xs.iter()
            .zip(ys)
            .enumerate()
            .map(|(i, (x, y))| {
                format!("{}{} {}", if i == 0 { "M" } else { "L" }, f(1, x_of(*x)), f(1, y_of(*y)))
            })
            .collect::<Vec<_>>()
            .join(" ")
    };

    let mut out = vec![format!(
        "<svg class=\"plot bits\" viewBox=\"0 0 {} {}\" width=\"100%\" \
         preserveAspectRatio=\"xMidYMid meet\" role=\"img\" aria-label=\"\
         bits of min-entropy accumulating against seconds\">",
        width, height
    )];
    // A gridline every 64 bits is twenty-one labels on a long pool. Pick the
    // coarsest step that still leaves five or more.
    let tick = [64i64, 128, 256, 512, 1024]
        .iter()
        .copied()
        .find(|t| hi / *t as f64 <= 8.0)
        .unwrap_or(1024);
    let mut v = 0i64;
    while v as f64 <= hi {
        let y = y_of(v as f64);
        out.push(format!(
            "<line class=\"grid\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/>",
            pad_l, f(1, y), width - pad_r, f(1, y)
        ));
        out.push(format!(
            "<text class=\"ylab\" x=\"{}\" y=\"{}\">{}</text>",
            pad_l - 8,
            f(1, y + 4.0),
            v
        ));
        v += tick;
    }
    out.push(format!(
        "<line class=\"target\" x1=\"{}\" y1=\"{}\" x2=\"{}\" y2=\"{}\"/>",
        pad_l,
        f(1, y_of(want as f64)),
        width - pad_r,
        f(1, y_of(want as f64))
    ));
    let every = ((n / 8 / 60).max(1) * 60).max(60);
    for t in (every..=n).step_by(every) {
        out.push(format!(
            "<text class=\"xlab\" x=\"{}\" y=\"{}\">{}s</text>",
            f(1, x_of(t)),
            height - 12,
            t
        ));
    }
    out.push(format!("<path class=\"peak\" d=\"{}\"/>", path(&model)));
    out.push(format!("<path class=\"mean\" d=\"{}\"/>", path(&meas)));
    out.push(format!(
        "<text class=\"ycap\" x=\"{}\" y=\"{}\">bits</text>",
        pad_l - 46,
        pad_t - 2
    ));
    out.push("</svg>".to_string());
    out.concat()
}

const RANDOM_CSS: &[&str] = &[
    ".hist .seen{fill:var(--accent)}",
    ".hist .modelled{fill:var(--dim);opacity:.45}",
    ".bits .target{stroke:var(--warn);stroke-width:1;stroke-dasharray:4 3}",
    ".plot .xcap{text-anchor:start;font-size:10px;letter-spacing:.05em;\
     text-transform:uppercase}",
    ".ledger{font-family:ui-monospace,SFMono-Regular,Menlo,monospace;\
     font-size:.78rem;word-break:break-all}",
    ".budget td:first-child{width:52%}",
    ".note{color:var(--dim);font-size:.85rem;max-width:68ch}",
];

/// The audit page: what the source is, what it is worth, and every line.
///
/// Written because the claim on the front page -- 256 bits out of decay --
/// is the one thing on it a reader cannot check by looking.
pub fn render_random_html(serial: &str, pools: &[entropy::Emission], title: &str, index: &str) -> String {
    let samples: Vec<u32> = pools.iter().flat_map(|p| p.counts.iter().copied()).collect();
    let budget = entropy_budget(&samples);
    let mut out = page_head(title, RANDOM_CSS);
    macro_rules! a {
        ($($arg:tt)*) => { out.push(format!($($arg)*)) };
    }
    a!("<h1>{}</h1>", esc(title));
    a!(
        "<p class=\"sub\">radbeeper {} &middot; counter {} &middot; {}{} &middot; generated {}</p>",
        VERSION,
        esc(serial),
        pools.len(),
        if pools.len() == 1 { " emission" } else { " emissions" },
        esc(&clock::format(now(), "%Y-%m-%d %H:%M"))
    );
    a!("<p class=\"lede\">The moment a nucleus decays is not determined by \
        anything, which makes a Geiger&ndash;M&uuml;ller counter the textbook \
        hardware entropy source. What is <em>not</em> textbook is how much \
        randomness survives the journey down a serial cable, and that is what \
        this page is for. \
        <a href=\"{}\">Back to the monitor</a>.</p>", esc(index));

    let b = match budget {
        Some(b) => b,
        None => {
            a!("<p>No emissions recorded yet.</p>");
            a!("</main></body></html>");
            return out.join("\n");
        }
    };

    // ---------------------------------------------------------- the link ---
    a!("<h2>What the serial link gives you</h2>");
    a!("<p class=\"note\">Everything below follows from one line of the \
        protocol. <code>&lt;HEARTBEAT1&gt;&gt;</code> makes the counter send \
        <b>two bytes once per second</b>, and those two bytes are a count: \
        how many arrivals landed in that second. The device drives the clock, \
        so there is no drift between its second and ours &mdash; but there is \
        also nothing finer. <b>One integer per second is the entire raw \
        material.</b></p>");
    a!("<div class=\"tablewrap\"><table class=\"budget\"><tbody>");
    // A pool that measured nothing has no "seconds for": the Python cannot
    // print that row at all (it formats None), so there is no byte to match.
    let secs = |v: Option<f64>| v.map(|s| f(0, s)).unwrap_or_else(|| "--".to_string());
    let rows: Vec<(String, String)> = vec![
        ("Link".into(), "115200 baud &mdash; not the limit, and never has been".into()),
        ("What arrives".into(), "2 bytes per second, masked to 14 bits of count".into()),
        ("Sampling resolution".into(), "<b>1 second</b>, the finest the protocol offers".into()),
        ("Samples measured here".into(), commas(b.n as i64)),
        ("Mean".into(), format!("{} counts per second", f(3, b.mean))),
        ("Variance".into(), format!("{} &mdash; Poisson requires this to equal the mean", f(3, b.var))),
        ("Variance / mean".into(), format!("<b>{}</b>, so the arrivals are over-dispersed", f(2, b.dispersion))),
        (
            "Most common value".into(),
            format!("{} counts, in {}% of seconds", b.freq.mode(), f(1, 100.0 * b.pmax)),
        ),
        (
            "Min-entropy, measured".into(),
            format!("<b>{} bits per second</b> (SP 800-90B, 99% bound)", f(3, b.measured)),
        ),
        (
            "Min-entropy, Poisson model".into(),
            format!("{} bits per second &mdash; {}&times; too generous", f(3, b.modelled), f(1, b.ratio)),
        ),
        (
            format!("Seconds for {} bits", ENTROPY_BITS),
            format!(
                "<b>{} s</b> measured, against {} s the model would have claimed",
                secs(b.seconds_for),
                secs(b.seconds_modelled)
            ),
        ),
    ];
    for (k, v) in rows {
        a!("<tr><td>{}</td><td>{}</td></tr>", k, v);
    }
    a!("</tbody></table></div>");

    // ------------------------------------------------- what it is not ---
    a!("<h2>Why the spectrum contributes nothing</h2>");
    a!("<p class=\"note\">The monitor accumulates a power spectrum of the same \
        per-second counts, and it would be easy to think the bits come partly \
        from there. They do not, and cannot. The FFT is a <b>deterministic \
        function</b> of the samples, and for any deterministic \
        <i>f</i>, H<sub>&infin;</sub>(<i>f</i>(X)) &le; H<sub>&infin;</sub>(X): \
        a transform moves information about, it does not make any. Worse, \
        these coefficients come from a mean-subtracted, Hann-tapered window, \
        so neighbouring bins are correlated by construction &mdash; bits \
        pulled from them would look beautiful and carry less than they \
        appear to. <b>The entropy is in the counts. The spectrum is a \
        view of them</b>, used as a health check: a peak means something \
        periodic is contaminating the arrivals, and the emission is marked \
        suspect.</p>");

    // --------------------------------------------------- the distribution ---
    a!("<h2>The counts, against the model that was assumed</h2>");
    a!("<div class=\"plotwrap\">");
    out.push(svg_hist(&b.freq, b.mean));
    a!("</div>");
    a!("<div class=\"legend\"><span><i style=\"border-color:var(--accent)\"></i>\
        observed</span><span><i style=\"border-color:var(--dim)\"></i>\
        Poisson at the same mean</span></div>");
    // Only bins the model expected to see AT ALL. The ratio at k=10 is six
    // figures, which is arithmetic on a denominator of nothing.
    let mut worst: Option<(u32, f64)> = None;
    for (k, count) in b.freq.counts.iter() {
        if *k < 3 {
            continue;
        }
        let o = *count as f64 / b.n as f64;
        let e = poisson_pmf(b.mean, *k);
        if e * b.n as f64 >= 1.0 {
            let r = o / e;
            if worst.map(|w| r > w.1).unwrap_or(true) {
                worst = Some((*k, r));
            }
        }
    }
    let empty = 100.0
        * (b.freq.get(0) as f64 / b.n as f64 / pymax(poisson_pmf(b.mean, 0), 1e-12) - 1.0);
    a!(
        "<p class=\"note\">Empty seconds are <b>{}% more common</b> than \
         Poisson allows, and the tail runs far past it{}. Both push in the \
         same direction: an over-dispersed distribution is piled onto its \
         mode, and the mode is exactly what min-entropy is about. That is why \
         the measured figure is smaller than the modelled one, not larger.",
        f(0, empty),
        match worst {
            Some((k, r)) => format!(
                " &mdash; {} counts in a second happens {}&times; more often \
                 than the model says it should",
                k,
                f(0, r)
            ),
            None => String::new(),
        }
    );

    // ----------------------------------------------------- accumulation ---
    let chart = svg_bits(&samples);
    if !chart.is_empty() {
        a!("<h2>How the bits accumulate</h2>");
        a!("<div class=\"plotwrap\">{}</div>", chart);
        a!("<div class=\"legend\"><span><i style=\"border-color:var(--accent)\">\
            </i>measured</span><span><i style=\"border-color:var(--warn)\">\
            </i>Poisson model</span></div>");
        a!("<p class=\"note\">Every second recorded here, run as one pool. The \
            measured line is flat at first because the confidence bound has \
            nothing to say until there are about a dozen samples; it then \
            straightens as the estimate settles, and where it crosses the \
            dashed target a line is emitted. The modelled line reaches that \
            height in roughly half the time, which is precisely the error \
            this replaced &mdash; and why the pools listed below, collected \
            while the model was believed, all stop short of it.</p>");
    }

    // ------------------------------------------------------- the ceiling ---
    let lam = b.mean;
    let per_ms = if lam > 0.0 { -log_2(1.0 - (-lam * 0.001).exp()) } else { 0.0 };
    a!("<h2>What the resolution costs</h2>");
    a!(
        "<p class=\"note\">Bucketing arrivals into whole seconds throws most of \
         the randomness away, and it is worth knowing how much. If the link \
         reported the <em>time</em> of each arrival rather than a count per \
         second, the gap between two arrivals would be exponential, and \
         quantised to a millisecond it would carry about \
         <b>{} bits per arrival</b> &mdash; roughly {} bits a second at \
         this background rate, against the {} actually available. The \
         counter's protocol has no such message: <code>&lt;GETCPS&gt;&gt;</code> \
         and the heartbeat both answer with a count, never a timestamp. So \
         this is not a limit of the physics or of the tube. <b>It is the \
         cost of the interface</b>, and the honest thing is to measure what \
         gets through it rather than to claim the ceiling.</p>",
        f(1, per_ms),
        f(0, per_ms * lam),
        f(2, b.measured)
    );

    // ---------------------------------------------------------- the audit ---
    a!("<h2>Every line, and the counts behind it</h2>");
    a!("<p class=\"note\">Reproducible is not the same as predictable. Each \
        emission is SHA-256 over a label, its sequence number, the second the \
        pool opened, and the counts &mdash; all of which are here, so anyone \
        can recompute a past line and see it was not invented. That says \
        nothing whatever about the next one, which comes from decays that \
        have not happened.</p>");
    a!("<pre>radbeeper random --check {}</pre>", esc(&format!("random-{}.tsv", serial)));
    // THE BITS COLUMN IS RECOMPUTED HERE, not read out of the file. Rows
    // written before the estimator changed hold a Poisson figure.
    a!("<div class=\"tablewrap\"><table><thead><tr>\
        <th>seq</th><th>opened</th><th>s</th><th>counts/s</th>\
        <th>bits, measured</th><th>Poisson</th><th>flat</th>\
        </tr></thead><tbody>");
    let mut stale = false;
    for p in pools.iter().rev() {
        let mut when = p.time.as_str();
        if when < "2000" {
            // Written by a build that put time.monotonic() where the wall
            // clock belonged, so every such row dates itself to 1969.
            when = "not kept";
            stale = true;
        }
        let measured = entropy::mcv_min_entropy(&p.counts) * p.counts.len() as f64;
        let modelled = entropy::poisson_min_entropy(p.rate) * p.counts.len() as f64;
        a!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td><b>{}</b></td><td>{}</td><td>{}</td></tr>",
            p.seq,
            esc(when),
            p.seconds,
            f(3, p.rate),
            f(0, measured),
            f(0, modelled),
            if p.flat { "yes" } else { "<b>no</b>" }
        );
        a!("<tr><td colspan=\"7\" class=\"ledger\">{}</td></tr>", esc(&entropy::group_hex(&p.hex)));
    }
    a!("</tbody></table></div>");
    if stale {
        a!("<p class=\"note\">Rows marked <b>not kept</b> were written by a \
            build that recorded <code>time.monotonic()</code> &mdash; seconds \
            since this machine booted &mdash; where the wall clock belonged, \
            so they dated themselves to 1969. The digest used the same \
            number, so those lines still recompute; it was the label that \
            was wrong, and it is fixed. Those rows also stopped collecting on \
            the Poisson figure, which is why their measured column falls \
            short of {} &mdash; they are the evidence, not a warning.</p>", ENTROPY_BITS);
    }
    a!("<p class=\"note\"><b>Treat this as a good physical entropy source, not \
        a certified one.</b> It has not been through a statistical test \
        battery, and {} bits of measured min-entropy is a claim about the \
        samples that were seen, not a proof about the output.</p>", ENTROPY_BITS);
    a!("<footer>Generated by <code>radbeeper export</code> \
        {}. <a href=\"{}\">Back to the monitor</a>. \
        &middot; MIT License &mdash; Copyright (c) 2026 Paul Richeson\
        </footer>", VERSION, esc(index));
    a!("</main></body></html>");
    out.join("\n")
}

// ------------------------------------------------------------------ export ---

/// What an export wrote.
pub struct Report {
    /// The index, as the path was given.
    pub index: PathBuf,
    pub counters: usize,
    pub rows: u64,
    /// The audit page and how many emissions it accounts for, if written.
    pub random: Option<(PathBuf, usize)>,
}

/// Build index.html -- and random.html beside it, when there are emissions
/// and `random_page` -- from the logs in `logs`.
///
/// The links on the page are relative to the page, so it works wherever it is
/// served from: a repository root on Pages, or a directory opened off a disk.
/// Screenshots are linked only when they are actually beside the page, so an
/// export into some other directory does not litter it with broken images.
pub fn export(
    logs: &Path,
    output: &Path,
    random_page: bool,
    cpm_per_usvh: f64,
    title: &str,
    random_output: Option<&Path>,
) -> io::Result<Report> {
    let mut counters = summarise(logs, cpm_per_usvh);
    let out = path_str(output);
    let out_dir = dirname(&abspath(&out));
    for (serial, path) in log::files(logs) {
        if let Some(c) = counters.get_mut(&serial) {
            let p = path_str(&path);
            c.links.push((basename(&p).to_string(), relpath(&p, &out_dir)));
        }
    }
    let sites = log::read_sites(logs);
    let mut shots = BTreeMap::new();
    for name in SHOTS {
        let ext = if name.ends_with("320") { ".gif" } else { ".png" };
        let rel = format!("docs/screenshots/{}{}", name, ext);
        if Path::new(&join(&out_dir, &rel)).exists() {
            shots.insert(name.to_string(), rel);
        }
    }
    // The audit page, when there is anything to audit. Written first so the
    // front page only links to it if it exists.
    let randoms: Vec<(String, Vec<entropy::Emission>)> = random_files(logs)
        .into_iter()
        .map(|(serial, path)| (serial, entropy::read_emissions(&path)))
        .filter(|(_, pools)| !pools.is_empty())
        .collect();
    let mut random = None;
    if random_page {
        if let Some((serial, pools)) = randoms.first() {
            let target = match random_output.filter(|p| !p.as_os_str().is_empty()) {
                Some(p) => path_str(p),
                None => join(&out_dir, "random.html"),
            };
            fs::write(&target, render_random_html(serial, pools, RANDOM_TITLE, basename(&out)))?;
            random = Some((target, pools.len()));
        }
    }
    let link = random.as_ref().map(|(p, _)| relpath(p, &out_dir));
    let html = render_html(&counters, &sites, cpm_per_usvh, title, &shots, link.as_deref());
    fs::write(output, html)?;
    Ok(Report {
        index: output.to_path_buf(),
        counters: counters.len(),
        rows: counters.values().map(|c| c.rows).sum(),
        random: random.map(|(p, n)| (PathBuf::from(p), n)),
    })
}

/// `radbeeper export`, as the command line runs it: the export, then the same
/// two lines the Python prints.
pub fn run(
    logs: &Path,
    output: &Path,
    random_page: bool,
    cpm_per_usvh: f64,
    title: &str,
    random_output: Option<&Path>,
) -> i32 {
    match export(logs, output, random_page, cpm_per_usvh, title, random_output) {
        Ok(r) => {
            println!(
                "radbeeper: {} -- {} counter{}, {} rows",
                r.index.display(),
                r.counters,
                if r.counters == 1 { "" } else { "s" },
                r.rows
            );
            if let Some((p, n)) = &r.random {
                println!(
                    "radbeeper: {} -- {} emission{}",
                    p.display(),
                    n,
                    if *n == 1 { "" } else { "s" }
                );
            }
            0
        }
        Err(e) => {
            eprintln!("radbeeper: export: {}", e);
            1
        }
    }
}

// ----------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_are_written_the_way_python_writes_them() {
        assert_eq!(commas(0), "0");
        assert_eq!(commas(999), "999");
        assert_eq!(commas(1000), "1,000");
        assert_eq!(commas(28544), "28,544");
        assert_eq!(commas(1234567), "1,234,567");
        assert_eq!(e0(0.0001), "1e-04");
        assert_eq!(e0(0.00001), "1e-05");
        assert_eq!(f(0, 2.5), "2", "half to even, as printf does");
        assert_eq!(f(1, 0.25), "0.2");
    }

    #[test]
    fn paths_are_worked_out_the_way_os_path_does() {
        assert_eq!(relpath("/a/b/c.tsv", "/a"), "b/c.tsv");
        assert_eq!(relpath("/a/logs/x.tsv", "/a/site"), "../logs/x.tsv");
        assert_eq!(relpath("/a", "/a"), ".");
        assert_eq!(abspath("/a/./b/../c"), "/a/c");
        assert_eq!(abspath("/"), "/");
        assert_eq!(dirname("/a/b/index.html"), "/a/b");
        assert_eq!(dirname("/index.html"), "/");
        assert_eq!(join("/", "random.html"), "/random.html");
        assert_eq!(basename("out/index.html"), "index.html");
    }

    #[test]
    fn a_tie_for_the_most_common_value_goes_to_the_first_seen() {
        assert_eq!(Freq::of(&[2, 1, 1, 2, 0]).mode(), 2);
        assert_eq!(Freq::of(&[1, 2, 2, 1]).mode(), 1);
    }

    #[test]
    fn the_compensated_sum_is_not_a_running_total() {
        let xs = [1e16, 1.0, -1e16];
        assert_eq!(py_sum(xs.iter().copied()), 1.0);
        assert_eq!(xs.iter().sum::<f64>(), 0.0);
    }

    #[test]
    fn an_axis_ceiling_is_a_round_number() {
        assert_eq!(nice_ceiling(0.0), 1.0);
        assert_eq!(nice_ceiling(37.0), 40.0);
        assert_eq!(nice_ceiling(100.0), 100.0);
        assert_eq!(nice_ceiling(101.0), 150.0);
    }

    #[test]
    fn a_gap_breaks_the_line() {
        let series = vec![
            (0.0, 40.0, Some(80.0)),
            (3600.0, 41.0, None),
            (7200.0 * 4.0, 39.0, Some(90.0)),
        ];
        let svg = svg_plot(&series);
        let mean = svg.split("class=\"mean\" d=\"").nth(1).unwrap();
        assert_eq!(mean.split('"').next().unwrap().matches('M').count(), 2);
    }
}
