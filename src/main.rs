// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
// radbeeper -- a GQ GMC Geiger-Muller counter on the desk. The read side,
// native: find it, ask what it is, and watch it.
//
// Everything that writes the log format -- service, backfill, export, site,
// random, hotplug -- is in the one-file Python program in the same repository
// and stays there while that format is still moving. Asking this binary for
// one of them says so rather than pretending.
mod counter;
mod serial;

use radbeeper::{analysis, clock, entropy, history, log};
use analysis::{
    bar_rows, level, spectrum_columns, Ladder, Level, Windows,
};
use std::collections::BTreeSet;
use std::io::{Read, Write};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BIG_ROWS: usize = 12;
const SERVICE_WAIT: f64 = 10.0;

// A twelve-row digit, six columns wide, with the three horizontal bars drawn
// TWO rows thick -- a one-row bar between three-row uprights reads as a
// scratch once the digit is this tall.
fn glyph(ch: char) -> Option<&'static [&'static str]> {
    Some(match ch {
        '0' => &["111111","111111","100001","100001","100001","100001","100001","100001","100001","100001","111111","111111"],
        '1' => &["000110","001110","010110","000110","000110","000110","000110","000110","000110","000110","011111","011111"],
        '2' => &["111111","111111","000001","000001","000001","111111","111111","100000","100000","100000","111111","111111"],
        '3' => &["111111","111111","000001","000001","000001","111111","111111","000001","000001","000001","111111","111111"],
        '4' => &["100001","100001","100001","100001","100001","111111","111111","000001","000001","000001","000001","000001"],
        '5' => &["111111","111111","100000","100000","100000","111111","111111","000001","000001","000001","111111","111111"],
        '6' => &["111111","111111","100000","100000","100000","111111","111111","100001","100001","100001","111111","111111"],
        '7' => &["111111","111111","000001","000010","000010","000100","000100","001000","001000","010000","010000","010000"],
        '8' => &["111111","111111","100001","100001","100001","111111","111111","100001","100001","100001","111111","111111"],
        '9' => &["111111","111111","100001","100001","100001","111111","111111","000001","000001","000001","111111","111111"],
        '.' => &["00","00","00","00","00","00","00","00","00","00","11","11"],
        '-' => &["000000","000000","000000","000000","000000","111111","111111","000000","000000","000000","000000","000000"],
        _ => return None,
    })
}

/// The column the big digits start at: clear of the header on the same row.
///
/// Three spaces of air, because a digit butted against the serial number
/// reads as part of it.
fn digits_left(head: &str) -> usize {
    head.chars().count() + 3
}

fn big_number(text: &str) -> Vec<String> {
    let mut rows = vec![String::new(); BIG_ROWS];
    for ch in text.chars() {
        if let Some(g) = glyph(ch) {
            for r in 0..BIG_ROWS {
                for bit in g[r].chars() {
                    rows[r].push(if bit == '1' { '█' } else { ' ' });
                }
                rows[r].push(' ');
            }
        }
    }
    rows
}

// ---------------------------------------------------------------- screen ---
struct Screen {
    saved: libc::termios,
}

impl Screen {
    fn enter() -> Screen {
        let mut saved: libc::termios = unsafe { std::mem::zeroed() };
        unsafe {
            libc::tcgetattr(0, &mut saved);
            let mut raw = saved;
            raw.c_lflag &= !(libc::ICANON | libc::ECHO);
            raw.c_cc[libc::VMIN] = 0;
            raw.c_cc[libc::VTIME] = 0;
            libc::tcsetattr(0, libc::TCSANOW, &raw);
        }
        print!("\x1b[?1049h\x1b[?25l");
        let _ = std::io::stdout().flush();
        Screen { saved }
    }

    fn size(&self) -> (usize, usize) {
        let mut ws: libc::winsize = unsafe { std::mem::zeroed() };
        if unsafe { libc::ioctl(1, libc::TIOCGWINSZ, &mut ws) } == 0 && ws.ws_col > 0 {
            (ws.ws_row as usize, ws.ws_col as usize)
        } else {
            (24, 80)
        }
    }

    fn quit_pressed(&self) -> bool {
        let mut b = [0u8; 8];
        match std::io::stdin().read(&mut b) {
            Ok(n) if n > 0 => b[..n].iter().any(|&c| c == b'q' || c == b'Q' || c == 27),
            _ => false,
        }
    }
}

impl Drop for Screen {
    fn drop(&mut self) {
        print!("\x1b[?25h\x1b[?1049l");
        let _ = std::io::stdout().flush();
        unsafe { libc::tcsetattr(0, libc::TCSANOW, &self.saved) };
    }
}

const DIM: &str = "\x1b[2m";
const BOLD: &str = "\x1b[1m";
const OFF: &str = "\x1b[0m";
const GREEN: &str = "\x1b[32m";
const YELLOW: &str = "\x1b[33m";
const RED: &str = "\x1b[31m";
const CYAN: &str = "\x1b[36m";

fn colour_for(l: Level) -> &'static str {
    match l {
        Level::Calm => GREEN,
        Level::Raised => YELLOW,
        Level::High => RED,
    }
}

fn at(row: usize, col: usize) -> String {
    format!("\x1b[{};{}H", row + 1, col + 1)
}

// ----------------------------------------------------------------- watch ---
/// The window `cpm` reports, and the one the monitor puts in big digits.
const CPM_WINDOW: f64 = 30.0;

/// RadBeeper's own 30-second average, counted here rather than asked for.
///
/// `<GETCPM>>` WAS THE OBVIOUS THING AND IT ANSWERS A DIFFERENT QUESTION. The
/// device keeps an internal rolling SIXTY-second count, and that is a
/// different time constant from anything else this program prints: `watch`
/// puts the 30 s window in the big digits, and `cpm` answering with a 60 s
/// number meant the two disagreed by more than the noise, with neither of
/// them wrong. One number, one time constant, and it is the one the rest of
/// the program already uses.
///
/// THE COST IS THIRTY SECONDS AND IT IS NOT HIDDEN. A window shows nothing
/// until it is full -- the same promise the monitor makes, for the same
/// reason -- so this counts for the whole span before printing anything, and
/// says so on stderr while it waits. stdout carries the answer alone, so it
/// still pipes.
fn cpm_cmd(c: &counter::Counter, cpm_per_usvh: f64) -> i32 {
    let mut w = Windows::new(&[CPM_WINDOW]);
    eprintln!("counting for {}s...", CPM_WINDOW as i64);
    c.heartbeat(true);
    let start = Instant::now();
    while w.average(CPM_WINDOW).is_none() {
        let counts = match c.next_sample(Duration::from_millis(2500)) {
            Some(v) => v as u32,
            None => break,
        };
        w.add(start.elapsed().as_secs_f64(), counts);
    }
    c.heartbeat(false);
    match w.average(CPM_WINDOW) {
        Some(n) => {
            println!("{:.1} CPM   {:.3} uSv/h", n, n / cpm_per_usvh);
            0
        }
        None => {
            // int(), not round(): the Python prints int(w.elapsed()) and
            // the two messages have to be the same string.
            eprintln!("the counter stopped talking after {}s",
                      w.elapsed() as i64);
            1
        }
    }
}

fn watch(c: &counter::Counter, spans: &[f64], cpm_per_usvh: f64,
         duration: Option<f64>, logs: Option<std::path::PathBuf>) {
    // So a `kill` stops the stream and puts the terminal back, as q does.
    install_stop_handler();
    let screen = Screen::enter();
    let mut w = Windows::new(spans);
    let mut ladder = Ladder::new();
    let mut pool = entropy::Entropy::default();
    // The drawn line and the moment it was drawn, kept across frames: a line
    // stays on screen until the pool has earned the next one.
    let mut shown: Option<(String, String)> = None;
    let mut suspect = false;
    c.heartbeat(true);
    let start = Instant::now();
    let mut out = String::with_capacity(16384);

    loop {
        let counts = match c.next_sample(Duration::from_millis(2500)) {
            Some(v) => v as u32,
            None => break,
        };
        if stopping() {
            break;
        }
        let when = start.elapsed().as_secs_f64();
        w.add(when, counts);
        ladder.add(counts);
        pool.add(counts);
        let spec = ladder.best();

        let (h, width) = screen.size();
        out.clear();
        out.push_str("\x1b[2J");
        // No title row: the program's name is the one thing on this screen
        // nobody needs telling. The firmware moves in beside the port.
        let head = format!(
            "{} @ {} baud   {}   serial {}",
            c.path, c.baud, c.version, c.serial_no
        );
        out.push_str(&format!(
            "{}{}{}{}",
            at(0, 0), DIM, head, OFF
        ));

        let mut row = 2usize;
        for &span in spans {
            match w.average(span) {
                None => {
                    let left = (span - w.elapsed()).max(0.0).round() as i64;
                    out.push_str(&format!(
                        "{}{}{:>5}s   filling, {}s to go{}",
                        at(row, 0), DIM, span as i64, left, OFF
                    ));
                }
                Some(cpm) => {
                    out.push_str(&format!(
                        "{}{}{:>5}s{}{}{}{:>8.1} CPM{}{}{:>8.3} uSv/h",
                        at(row, 0), DIM, span as i64, OFF,
                        at(row, 7), colour_for(level(cpm)), cpm, OFF,
                        at(row, 23), cpm / cpm_per_usvh
                    ));
                }
            }
            row += 1;
        }
        row += 1;
        out.push_str(&format!(
            "{}{}now{}  {} counts this second{}{}run{}  {} counts in {}s",
            at(row, 0), BOLD, OFF, counts,
            at(row + 1, 0), DIM, OFF, w.total, w.elapsed().round() as i64
        ));
        // THREE, NOT TWO. `row` is still on the "now" line here -- "run" was
        // drawn at row + 1 without moving it -- so clearing the pair and
        // leaving the blank row after it costs three. Two put this monitor's
        // chart one row above the Python's for the whole of the port, which
        // nothing caught because both were only ever read on their own.
        row += 3;

        // The number, big, to the right of everything above.
        let headline = w.average(30.0).or_else(|| w.average(*spans.last().unwrap()));
        let digits = big_number(&match headline {
            Some(v) => format!("{:.0}", v),
            None => "--".to_string(),
        });
        let wide = digits.iter().map(|d| d.chars().count()).max().unwrap_or(0);
        // Clear of the header, which is on the row the digits start on. This
        // was a constant 54 and the header outgrew it: a serial is fourteen
        // characters and the firmware sits beside the port, so the digits
        // were landing on top of the counter's own name. Measured, not
        // guessed -- the same fix the Python carries.
        let left = digits_left(&head);
        if width > left + wide + 6 && h > BIG_ROWS {
            let tint = headline.map(|v| colour_for(level(v))).unwrap_or(DIM);
            for (i, d) in digits.iter().enumerate() {
                out.push_str(&format!("{}{}{}{}", at(i, left), tint, d, OFF));
            }
        }

        // Three rows of air, then the counts.
        row += 3;
        let counts_rows = if h > row + 12 { 5 } else { 1 };
        let vals: Vec<f64> = w.samples.iter().map(|&(_, c)| c as f64).collect();
        let tail: Vec<f64> = vals.iter().rev().take(width - 1).rev().cloned().collect();
        for (i, line) in bar_rows(&vals, width - 1, counts_rows).iter().enumerate() {
            out.push_str(&at(row + i, 0));
            let mut pen = "";
            for (x, &g) in line.iter().enumerate() {
                if g == ' ' {
                    out.push(' ');
                    continue;
                }
                let want = colour_for(level(tail.get(x).cloned().unwrap_or(0.0) * 60.0));
                if want != pen {
                    out.push_str(want);
                    pen = want;
                }
                out.push(g);
            }
            out.push_str(OFF);
        }
        row += counts_rows + 1;

        // The spectrum: flat is the good answer.
        let mut footer = String::new();
        let rel = spec.relative();
        if rel.is_empty() {
            footer = format!(
                "spectrum   accumulating, {}s to the first window of {}",
                spec.wait(), spec.window
            );
        } else if h > row + 4 {
            let cols = spectrum_columns(&rel, width - 1);
            let (top, where_) = spec.loudest();
            let luck = spec.chance_max();
            footer = if top < luck * 1.25 {
                format!(
                    "spectrum   flat -- arrivals look random, as decay should ({} windows)",
                    spec.runs
                )
            } else {
                format!(
                    "spectrum   peak at {:.0}s, {:.1}x the mean (chance gives {:.1}x), {:.1} sigma ({} windows)",
                    spec.period(where_), top, luck, spec.sigma(top), spec.runs
                )
            };
            let bars = if h > row + 9 { 5 } else { 1 };
            for (i, line) in bar_rows(&cols, cols.len(), bars).iter().enumerate() {
                out.push_str(&at(row + i, 0));
                let mut pen = "";
                for (x, &g) in line.iter().enumerate() {
                    if g == ' ' {
                        out.push(' ');
                        continue;
                    }
                    let v = cols[x];
                    let want = if v >= luck * 2.0 { RED } else if v >= luck * 1.25 { YELLOW } else { CYAN };
                    if want != pen {
                        out.push_str(want);
                        pen = want;
                    }
                    out.push(g);
                }
                out.push_str(OFF);
            }
            row += bars;
            let lo = format!("{}s", spec.window);
            let gap = (width - 1).saturating_sub(lo.len() + 2);
            out.push_str(&format!("{}{}{}{}2s{}", at(row, 0), DIM, lo, " ".repeat(gap), OFF));
        }

        // The rotating line: 256 bits of decay, when the pool has earned
        // them. See the entropy module for what "earned" is doing there --
        // it is measured from the samples, not modelled from their mean.
        if h > row + 2 && width > 84 {
            row += 1;
            if pool.ready() {
                let (top, _) = spec.loudest();
                suspect = top > 0.0 && top >= spec.chance_max() * 1.25;
                let (text, record) = pool.draw();
                shown = Some((text, clock::format(clock::now(), "%H:%M:%S")));
                if let Some(d) = logs.as_ref() {
                    let _ = entropy::write_record(d, &record, &c.serial_no, suspect);
                }
            }
            match &shown {
                Some((text, at_time)) => {
                    out.push_str(&format!("{}{}random   {}{}",
                                          at(row, 0), CYAN,
                                          entropy::group_hex(text), OFF));
                    if h > row + 2 {
                        // THE COUNTDOWN KEEPS RUNNING. The line stayed on
                        // screen with no indication of whether the next one
                        // was a minute away or eight, which is the one thing
                        // somebody watching it wants to know.
                        let note = format!(
                            "{} bits from decay at {}   {}{}",
                            entropy::ENTROPY_BITS as i64, at_time,
                            entropy::pool_status(&pool, "next in "),
                            if suspect {
                                "  -- SPECTRUM NOT FLAT, treat as suspect"
                            } else {
                                ""
                            }
                        );
                        out.push_str(&format!("{}{}{}{}", at(row + 1, 9),
                                              if suspect { YELLOW } else { DIM },
                                              note, OFF));
                    }
                }
                None => out.push_str(&format!(
                    "{}{}random   {}{}", at(row, 0), DIM,
                    entropy::pool_status(&pool, "next in "), OFF
                )),
            }
        }

        if h >= 2 {
            out.push_str(&format!("{}{}{}{}", at(h - 2, 0), DIM, footer, OFF));
            out.push_str(&format!("{}{}q to quit{}", at(h - 1, 0), DIM, OFF));
        }
        print!("{}", out);
        let _ = std::io::stdout().flush();

        if screen.quit_pressed() {
            break;
        }
        if let Some(d) = duration {
            if w.elapsed() >= d {
                break;
            }
        }
    }
    c.heartbeat(false);
}

// ------------------------------------------------------------------ main ---

/// Log to disk, a row every `every` seconds, until told to stop.
///
/// WAITING IS NOT RETRYING, and what separates them is the device. An absent
/// counter does not become present because a daemon asked again, so that case
/// writes down why and exits 0 -- a stopped service, not a crash loop. A port
/// that is present but LOCKED is the opposite: the counter is right there,
/// somebody is watching it in a monitor, and they will close it. One open()
/// every ten seconds picks the log back up the moment they do.
fn service(spans: &[f64], every: f64, duration: Option<f64>,
           device: Option<&str>, baud: Option<u32>,
           logs: Option<std::path::PathBuf>,
           backfill: Option<(usize, f64)>) -> i32 {
    install_stop_handler();

    let started = clock::now();
    let mut waiting = false;
    let c = loop {
        match counter::find(device, baud) {
            Ok(c) => break c,
            Err(e) => {
                if !e.busy {
                    let path = log::write_status(&format!("dormant: {}", e.reason));
                    println!("radbeeper: dormant -- {}", e.reason);
                    for line in e.detail.lines() {
                        println!("    {}", line);
                    }
                    println!("    status: {}", path.display());
                    println!("    it will look again at the next boot, or when you \
                              run: rc-service radbeeper start");
                    return 0;
                }
                if !waiting {
                    waiting = true;
                    log::write_status(&format!("waiting: {}", e.reason));
                    println!("radbeeper: waiting -- {}", e.reason);
                    println!("    Logging starts by itself when the port is free.");
                }
                let mut slept = 0.0;
                while slept < SERVICE_WAIT {
                    if stopping() {
                        return 0;
                    }
                    if duration.map(|d| clock::now() - started >= d).unwrap_or(false) {
                        return 0;
                    }
                    std::thread::sleep(Duration::from_millis(500));
                    slept += 0.5;
                }
            }
        }
    };

    // --logs is what makes this path testable at all. Without it the only
    // way to exercise the logger is against the machine's real log.
    let dir = logs.unwrap_or_else(log::state_dir);
    let _ = std::fs::create_dir_all(&dir);

    // BEFORE ANYTHING IS APPENDED, as the Python does: backfilled rows belong
    // in the past and the merge rewrites the file. The service starts when a
    // counter is plugged in or the machine comes up, which is exactly when
    // the counter has been recording somewhere this log was not. The status
    // file says so, because reading the flash takes a while.
    if let Some((bytes, max_gap)) = backfill {
        log::write_status("backfilling from the counter's history");
        let offset = measure_clock_offset(&c).unwrap_or(0.0);
        let blob = read_history_tail(&c, bytes, false);
        if blob.is_empty() {
            println!("radbeeper: backfill skipped -- the counter returned no history");
        } else {
            let sites = log::read_sites(&dir);
            let r = history::backfill(&blob, spans, every, max_gap, offset, &dir,
                                      Some(c.serial_no.as_str()), &sites, None);
            println!("radbeeper: backfill -- {} samples, {} rows, {} added, {} already logged",
                     r.samples, r.rows, r.added, r.clashed);
        }
    }

    log::write_status(&format!("monitoring {} ({})", c.path, c.version));
    let mut w = Windows::new(spans);
    let mut iv = log::Interval::new(spans.len());
    let mut out = log::Writer::new(spans, dir.clone(), Some(c.serial_no.clone()), every);

    // The site is re-read whenever sites.tsv changes, so `radbeeper site`
    // takes effect on the next row rather than at the next restart.
    let mut sites = log::read_sites(&dir);
    let mut sites_mtime = mtime_of(&dir.join("sites.tsv"));

    let here = log::site_at(&c.serial_no, clock::now(), &sites);
    println!("radbeeper: monitoring {} -- {}", c.path, c.version);
    println!("radbeeper: counter {} at {}", c.serial_no,
             here.unwrap_or_else(|| "an unrecorded place".to_string()));
    println!("radbeeper: logging to {}, a row every {}s",
             log::path(clock::now(), &dir, Some(&c.serial_no)).display(),
             log::g(every));

    // THE COUNTER HAS TO BE ASKED TO TALK. Without this the first read times
    // out, the loop breaks on the spot and the service exits 0 having written
    // nothing -- which is what it did, silently, until a test asked for the
    // file afterwards. It appeared to work only because a `watch` killed
    // without its cleanup leaves the counter streaming, and the next process
    // inherits that.
    c.heartbeat(true);
    let mut due: Option<f64> = None;
    loop {
        let counts = match c.next_sample(Duration::from_millis(2500)) {
            Some(v) => v as u32,
            None => break,
        };
        let when = clock::now();
        w.add(when, counts);
        let averages: Vec<Option<f64>> =
            spans.iter().map(|s| w.average(*s)).collect();
        iv.add(counts, &averages, 1.0);
        if due.is_none() {
            due = Some(when + every);
        }
        // One write and one flush per interval instead of per second: at the
        // default that is two syscalls a minute rather than a hundred and
        // twenty, which is the whole difference on a Pi Zero logging to an SD
        // card. Nothing is buffered up to pay for it.
        if when >= due.unwrap() {
            let now = clock::now();
            let (m, s) = (mtime_of(&dir.join("sites.tsv")), &mut sites);
            if m != sites_mtime {
                sites_mtime = m;
                *s = log::read_sites(&dir);
            }
            let site = log::site_at(&c.serial_no, now, &sites).unwrap_or_default();
            let line = log::row(now, iv.cps(), iv.counts, iv.seconds,
                                &averages, &iv.peaks, log::SRC_LIVE, &site);
            if let Err(e) = out.write(now, &line) {
                eprintln!("radbeeper: could not write the log -- {}", e);
                break;
            }
            iv.reset();
            due = Some(when + every);
        }
        if stopping() {
            break;
        }
        // --duration is what makes this path testable at all: without it the
        // only way to exercise the logger is to start a daemon and kill it,
        // which is not something a test suite should do.
        if duration.map(|d| w.elapsed() >= d).unwrap_or(false) {
            break;
        }
    }

    // Whatever the last interval collected is worth keeping: a service
    // stopped four seconds after a spike should still have the spike on
    // disk, and the seconds column says the row is short.
    if iv.seconds > 0.0 {
        let now = clock::now();
        let averages: Vec<Option<f64>> =
            spans.iter().map(|s| w.average(*s)).collect();
        let site = log::site_at(&c.serial_no, now, &sites).unwrap_or_default();
        let line = log::row(now, iv.cps(), iv.counts, iv.seconds,
                            &averages, &iv.peaks, log::SRC_LIVE, &site);
        let _ = out.write(now, &line);
    }
    out.close();
    c.heartbeat(false);
    log::write_status("stopped");
    0
}

fn mtime_of(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.mtime() as u64)
}

static STOP: AtomicBool = AtomicBool::new(false);

/// SIGTERM and SIGINT set a flag; the loop notices between samples.
///
/// A signal handler must not allocate, take a lock or call anything that
/// might, so it does the one thing that is safe: stores to an atomic the loop
/// already reads. Stopping between samples rather than mid-write is also what
/// keeps a half-written row off the disk.
fn install_stop_handler() {
    extern "C" fn handler(_sig: libc::c_int) {
        STOP.store(true, Ordering::Relaxed);
    }
    let f: extern "C" fn(libc::c_int) = handler;
    unsafe {
        libc::signal(libc::SIGTERM, f as usize as libc::sighandler_t);
        libc::signal(libc::SIGINT, f as usize as libc::sighandler_t);
    }
}

fn stopping() -> bool {
    STOP.load(Ordering::Relaxed)
}


/// Collect until the pool has earned the bits, then print one line.
fn random(spans: &[f64], duration: Option<f64>, device: Option<&str>,
          baud: Option<u32>, logs: Option<std::path::PathBuf>) -> i32 {
    install_stop_handler();
    let c = match counter::find(device, baud) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}: {}",
                      if e.busy { "port busy" } else { "no counter" }, e.reason);
            return 1;
        }
    };
    let want = entropy::ENTROPY_BITS;
    let mut pool = entropy::Entropy::new(want);
    let mut ladder = Ladder::new();
    println!("radbeeper: collecting {} bits from {}", want as i64, c.path);
    c.heartbeat(true);
    let started = clock::now();
    while !pool.ready() {
        let counts = match c.next_sample(Duration::from_millis(2500)) {
            Some(v) => v as u32,
            None => break,
        };
        pool.add(counts);
        ladder.add(counts);
        if stopping() {
            break;
        }
        if duration.map(|d| clock::now() - started >= d).unwrap_or(false) {
            break;
        }
    }
    c.heartbeat(false);
    if !pool.ready() {
        eprintln!("radbeeper: only {:.0} of {} bits -- {}",
                  pool.bits(), want as i64,
                  if duration.is_some() { "stopped early" } else { "interrupted" });
        return 1;
    }
    let spec = ladder.best();
    let (top, _) = spec.loudest();
    let suspect = top > 0.0 && top >= spec.chance_max() * 1.25;
    let modelled = pool.model_bits();
    let seconds = pool.counts.len();
    let rate = pool.rate();
    let (text, record) = pool.draw();
    println!();
    println!("{}", entropy::group_hex(&text));
    println!();
    println!("  {} bits, min-entropy {:.0} measured, from {} seconds at {:.2} \
              counts/s", want as i64, record.bits, seconds, rate);
    // The number the old Poisson model would have printed, beside the one the
    // samples actually support. Over-dispersed arrivals make the gap large.
    println!("  {:.0} bits is what a Poisson model would have claimed for the \
              same {} seconds", modelled, seconds);
    println!("  spectrum {}", if suspect {
        "NOT FLAT -- something periodic; treat as suspect"
    } else {
        "flat -- the source looks like decay"
    });
    let dir = logs.unwrap_or_else(log::state_dir);
    match entropy::write_record(&dir, &record, &c.serial_no, suspect) {
        Ok(p) => println!("  recorded in {}", p.display()),
        Err(e) => println!("  not recorded: {}", e),
    }
    let _ = spans;
    0
}

/// Recompute every emission from the counts written beside it.
///
/// This is the whole of the audit trail's promise: a line that cannot be
/// recomputed from its own counts was invented, and one that can was not. It
/// proves nothing about the NEXT line, which is the point -- that one comes
/// from decays that have not happened.
fn check_random(path: &std::path::Path) -> i32 {
    let pools = entropy::read_emissions(path);
    if pools.is_empty() {
        eprintln!("radbeeper: no emissions in {}", path.display());
        return 1;
    }
    let mut bad = 0;
    for p in &pools {
        let started = clock::parse_stamp(&p.time).unwrap_or(0.0);
        let ok = entropy::check_record(p.seq, started, &p.counts, &p.hex);
        if !ok {
            bad += 1;
        }
        println!("  seq {:<4} {:<20} {:>4}s  {:.3} bits/s  {}",
                 p.seq, p.time, p.seconds,
                 entropy::mcv_min_entropy(&p.counts),
                 if ok { "recomputes" } else { "DOES NOT RECOMPUTE" });
    }
    println!("radbeeper: {} of {} emissions recompute from their own counts",
             pools.len() - bad, pools.len());
    if bad > 0 { 1 } else { 0 }
}


/// The time of the first timestamp in a probe read at `address`.
fn first_mark_time(c: &counter::Counter, address: usize, probe: usize)
    -> Option<f64>
{
    let chunk = c.read_history(address, probe);
    history::raw(&chunk).find_map(|r| match r {
        history::Raw::Mark { when, .. } => Some(when),
        _ => None,
    })
}

/// (newest byte address, whether the flash has wrapped).
///
/// Pulling a full megabyte over 115200 baud takes ten minutes and the rows a
/// backfill wants are the NEWEST, so the first job is to find where they are.
/// Two shapes of flash, two searches.
///
/// NOT YET FULL: writing runs forward from zero and the rest is 0xFF, so
/// eleven probes bisect for where the 0xFF starts.
///
/// ALREADY WRAPPED, which is what the counter on the bench does -- a megabyte
/// of ring with no unwritten byte in it. The newest sample is immediately
/// before the write pointer and the oldest immediately after, so the flash
/// reads as one long climb in time with exactly one step backwards in it, and
/// that step bisects. Getting this wrong is not a small error: reading the
/// physical tail of a wrapped ring hands back the OLDEST hours while claiming
/// they are the newest, and a backfill would file last week under this
/// afternoon.
fn find_history_end(c: &counter::Counter, size: usize) -> (usize, bool) {
    const PROBE: usize = 2048;
    let tail = c.read_history(size.saturating_sub(PROBE), PROBE);
    if !tail.iter().any(|b| *b != 0xFF) {
        if !c.read_history(0, PROBE).iter().any(|b| *b != 0xFF) {
            return (0, false);
        }
        let (mut lo, mut hi) = (0usize, size);
        while hi - lo > PROBE {
            let mid = (lo + hi) / 2;
            if c.read_history(mid, PROBE).iter().any(|b| *b != 0xFF) {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        // Bisection lands within a probe of the truth; the last step reads
        // that window and takes the exact byte, so the tail read does not
        // carry a kilobyte of 0xFF with it.
        let window = c.read_history(lo, PROBE * 2);
        let last = window.iter().rposition(|b| *b != 0xFF);
        return (last.map(|j| lo + j + 1).unwrap_or(lo), false);
    }
    let start = match first_mark_time(c, 0, PROBE) {
        Some(t) => t,
        None => return (size, true), // no marks to steer by; take it as it lies
    };
    let (mut lo, mut hi) = (0usize, size);
    while hi - lo > PROBE {
        let mid = (lo + hi) / 2;
        match first_mark_time(c, mid, PROBE) {
            Some(seen) if seen < start => hi = mid,
            _ => lo = mid,
        }
    }
    let window = c.read_history(lo, PROBE * 2);
    for r in history::raw(&window) {
        if let history::Raw::Mark { off, when } = r {
            if when < start {
                return (lo + off, true);
            }
        }
    }
    (lo + PROBE, true)
}

/// The newest `want` bytes of the flash, in the order they were recorded.
///
/// In a wrapped ring the newest bytes end at the write pointer and, if more
/// are asked for than lie before it, continue from the physical end of the
/// chip. What comes back is always chronological, so nothing downstream has
/// to know the flash is a circle.
fn read_history_tail(c: &counter::Counter, want: usize, quiet: bool) -> Vec<u8> {
    let size = c.flash_size();
    if size == 0 {
        return Vec::new();
    }
    let (end, wrapped) = find_history_end(c, size);
    if end == 0 {
        return Vec::new();
    }
    if !quiet {
        println!("radbeeper: newest history at {} KiB of {} KiB{}; reading {} KiB",
                 end / 1024, size / 1024,
                 if wrapped { " (wrapped)" } else { "" },
                 want.min(size) / 1024);
    }
    if want <= end {
        return c.read_history(end - want, want);
    }
    let head = c.read_history(0, end);
    if !wrapped {
        return head;
    }
    let over = (want - end).min(size - end);
    let mut out = c.read_history(size - over, over);
    out.extend_from_slice(&head);
    out
}

/// Seconds to add to the counter's timestamps to land on this clock.
///
/// The counter's RTC is not this machine's. On the unit here it reads about
/// half an hour behind, which is six hundred slots at the default row
/// spacing: backfilling without this correction would file four hours of
/// recording under the wrong four hours. Timed to the counter's tick rather
/// than read once, because one reading is only good to the whole second it
/// names -- see `Counter::clock_ahead`.
fn measure_clock_offset(c: &counter::Counter) -> Option<f64> {
    c.clock_ahead().map(|o| -o.ahead)
}

/// "111.4 s ahead of this machine (±0.02 s)", or "matches" when it does.
fn describe_offset(o: &counter::ClockOffset) -> String {
    let within = if o.within < 0.1 {
        format!("\u{b1}{:.2} s", o.within)
    } else {
        format!("\u{b1}{:.1} s", o.within)
    };
    if o.ahead.abs() <= o.within.max(0.05) {
        return format!("matches this machine ({})", within);
    }
    let (n, way) = if o.ahead > 0.0 { (o.ahead, "ahead of") } else { (-o.ahead, "behind") };
    let size = if n >= 10.0 { format!("{:.1} s", n) } else { format!("{:.2} s", n) };
    format!("{} {} this machine ({})", size, way, within)
}

/// Whether the kernel believes this machine's clock is disciplined by NTP.
///
/// Setting the counter from this clock copies its error, so a clock nothing
/// is steering is worth a line of warning. adjtimex with no modes changes
/// nothing; it reports TIME_ERROR while the kernel's clock is unsynchronised.
fn system_clock_synced() -> bool {
    let mut t: libc::timex = unsafe { std::mem::zeroed() };
    unsafe { libc::adjtimex(&mut t) != libc::TIME_ERROR }
}

/// `radbeeper clock`, and `--set` to correct it from this machine.
///
/// THE SET IS AIMED AT A WHOLE SECOND. <SETDATETIME>> carries whole seconds,
/// so it is sent as this machine's clock reaches the second it names, and
/// the result is measured the same way `probe` measures. If it landed off --
/// the counter's own latency, or a firmware that keeps its sub-second phase
/// -- the send is moved by what was measured and tried once more, and
/// whatever the second measurement says is what gets printed.
fn clock_cmd(c: &counter::Counter, set: bool) -> i32 {
    let before = match c.clock_ahead() {
        Some(o) => o,
        None => {
            eprintln!("radbeeper: the counter did not answer <GETDATETIME>>");
            return 1;
        }
    };
    let synced = system_clock_synced();
    println!("its clock      {}", clock::format(before.theirs, "%Y-%m-%d %H:%M:%S"));
    println!("               {}", describe_offset(&before));
    println!("this machine   {}", if synced {
        "synchronised (the kernel says NTP is steering it)"
    } else {
        "NOT synchronised -- nothing is steering this clock"
    });
    if !set {
        if before.ahead.abs() > 1.0 {
            println!("               radbeeper clock --set  corrects it from this machine");
        }
        return 0;
    }
    if !synced {
        println!("               setting anyway: the counter will be as right as this machine is");
    }

    let mut lead = 0.0f64;
    let mut after = None;
    for _ in 0..2 {
        let now = clock::now();
        let target = now.floor() + 2.0;
        let wait = target - lead - clock::now();
        if wait > 0.0 {
            std::thread::sleep(Duration::from_secs_f64(wait));
        }
        if !c.set_clock(target) {
            eprintln!("radbeeper: the counter did not acknowledge <SETDATETIME>>");
            return 1;
        }
        after = c.clock_ahead();
        match after {
            Some(o) if o.ahead.abs() > o.within.max(0.05) => {
                lead = (lead - o.ahead).clamp(-0.9, 0.9);
            }
            _ => break,
        }
    }
    let after = match after {
        Some(o) => o,
        None => {
            eprintln!("radbeeper: set, but the counter did not answer the check");
            return 1;
        }
    };
    println!("set            {}", clock::format(after.theirs, "%Y-%m-%d %H:%M:%S"));
    println!("               {}", describe_offset(&after));
    if before.ahead.abs() >= 1.0 {
        // The flash does not rewrite itself. A backfill measures one offset
        // and applies it to the whole tail it reads, so a tail that spans
        // this moment has two clocks in it and the older part is out by
        // what was just corrected.
        println!();
        println!("  history the counter recorded before now carries the old clock, {:.0} s",
                 before.ahead.abs());
        println!("  {}. A backfill applies one offset to everything it reads, so rows",
                 if before.ahead > 0.0 { "ahead" } else { "behind" });
        println!("  it rebuilds from before this moment will be out by that much.");
    }
    0
}

#[allow(clippy::too_many_arguments)]
fn backfill_cmd(spans: &[f64], every: f64, max_gap: f64, bytes: usize,
                device: Option<&str>, baud: Option<u32>,
                logs: Option<std::path::PathBuf>, image: Option<&std::path::Path>,
                serial_arg: Option<&str>, one_file: Option<&str>)
    -> i32
{
    let dir = logs.unwrap_or_else(log::state_dir);
    let _ = std::fs::create_dir_all(&dir);
    let (blob, offset, serial) = match image {
        // An image on disk needs no counter, which is what makes a decoder
        // fix testable against last week's dump.
        Some(p) => match std::fs::read(p) {
            // A dumped image does not carry its own serial, and rows that
            // cannot say which counter they came from are half a measurement.
            Ok(b) => match serial_arg {
                Some(sn) => (b, 0.0, sn.to_string()),
                None => {
                    eprintln!("radbeeper: --serial is how a dumped image says \
                               which counter it came from");
                    return 1;
                }
            },
            Err(e) => {
                eprintln!("radbeeper: cannot read {} -- {}", p.display(), e);
                return 1;
            }
        },
        None => {
            let c = match counter::find(device, baud) {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("no counter: {}", e.reason);
                    return 1;
                }
            };
            let off = measure_clock_offset(&c).unwrap_or(0.0);
            let b = read_history_tail(&c, bytes, false);
            (b, off, c.serial_no.clone())
        }
    };
    if blob.is_empty() {
        eprintln!("radbeeper: nothing to read");
        return 1;
    }
    let sites = log::read_sites(&dir);
    let serial_opt = if serial.is_empty() { None } else { Some(serial.as_str()) };
    let r = history::backfill(&blob, spans, every, max_gap, offset, &dir,
                              serial_opt, &sites, one_file.map(std::path::Path::new));
    if r.samples == 0 {
        println!("radbeeper: no placeable samples in the history read");
        return 0;
    }
    println!("radbeeper: {} samples, {} rows, {} added, {} already logged",
             r.samples, r.rows, r.added, r.clashed);
    if let (Some(a), Some(b)) = (r.first, r.last) {
        println!("           {} .. {}", clock::stamp(a), clock::stamp(b));
    }
    println!("           clock offset {:+.0}s, {} hole{}", offset, r.holes,
             if r.holes == 1 { "" } else { "s" });
    for f in &r.files {
        println!("           {}", f.display());
    }
    0
}

fn log_cmd(action: &str, bytes: Option<usize>, out_stem: Option<&str>,
           device: Option<&str>, baud: Option<u32>) -> i32 {
    let c = match counter::find(device, baud) {
        Ok(c) => c,
        Err(e) => {
            eprintln!("no counter: {}", e.reason);
            return 1;
        }
    };
    if action == "info" {
        println!("model     {}", c.model());
        println!("flash     {} KiB", c.flash_size() / 1024);
        println!("pull it   radbeeper log pull -o FILE");
        return 0;
    }
    let size = bytes.unwrap_or_else(|| c.flash_size());
    let stem = match out_stem {
        Some(s) => s.to_string(),
        None => log::state_dir()
            .join(format!("history-{}", clock::format(clock::now(), "%Y%m%d-%H%M%S")))
            .display()
            .to_string(),
    };
    println!("reading {} KiB of history from {}", size / 1024, c.path);
    let got = c.read_history(0, size);
    if got.is_empty() {
        eprintln!("the counter returned nothing");
        return 1;
    }
    let raw_path = format!("{}.bin", stem);
    let csv_path = format!("{}.csv", stem);
    // THE RAW BYTES FIRST, ALWAYS. A decoder that mis-reads a marker costs a
    // bad CSV and never the data, and a better decoder can be run over the
    // same image later. That rule is what found both corrections to GQ's
    // published format.
    if let Err(e) = std::fs::write(&raw_path, &got) {
        eprintln!("radbeeper: cannot write {} -- {}", raw_path, e);
        return 1;
    }
    println!("raw    {}  ({} KiB)", raw_path, got.len() / 1024);
    let mut text = String::from("offset,time,interval_s,count,note\n");
    let mut rows = 0usize;
    for r in history::records(&got) {
        text.push_str(&format!(
            "{},{},{},{},{}\n",
            r.off,
            r.when.map(clock::stamp).unwrap_or_default(),
            r.dt.map(|d| format!("{:.4}", d)).unwrap_or_default(),
            r.count.map(|c| c.to_string()).unwrap_or_default(),
            r.note.replace(',', " ")
        ));
        rows += 1;
    }
    if let Err(e) = std::fs::write(&csv_path, text) {
        eprintln!("radbeeper: cannot write {} -- {}", csv_path, e);
        return 1;
    }
    println!("csv    {}  ({} rows)", csv_path, rows);
    println!("       times are the COUNTER's clock; interval_s is the measured");
    println!("       spacing of its samples. radbeeper backfill corrects both.");
    if rows == 0 {
        println!("nothing was recorded -- the counter's history may be empty.");
    }
    0
}

fn parse_spans(text: &str) -> Option<Vec<f64>> {
    let mut out = Vec::new();
    for part in text.split(',') {
        let v: f64 = part.trim().parse().ok()?;
        if v <= 0.0 {
            return None;
        }
        out.push(v);
    }
    (!out.is_empty()).then_some(out)
}

fn usage() {
    println!("radbeeper {} -- a GQ GMC counter on the desk (Rust build)", VERSION);
    println!();
    println!("  radbeeper probe            find the counter and say what it is");
    println!("  radbeeper cpm              the counter's own CPM, once");
    println!("  radbeeper clock [--set]    its clock against this machine's, or correct it");
    println!("  radbeeper watch            the monitor");
    println!("  radbeeper service          log to disk, a row every 30 seconds");
    println!("  radbeeper random           256 bits of hex, out of decay timing");
    println!("  radbeeper random --check F recompute every line in an emission log");
    println!("  radbeeper backfill         fill the log's gaps from the counter's flash");
    println!("  radbeeper log info|pull    what history it holds, or download it");
    println!("  radbeeper hotplug          sit in the session, open the monitor on plug-in");
    println!();
    println!("  -d, --device PATH          serial port (default: search /dev)");
    println!("  -b, --baud RATE            baud (default: try 115200 then 57600)");
    println!("      --spans 3,30,300,3000,30000  averaging windows, seconds");
    println!("      --cpm-per-usvh N       tube factor (default {})", counter::DEFAULT_CPM_PER_USVH);
    println!("      --duration SECONDS     stop after this long");
    println!("      --log-every SECONDS    row spacing for service (default {})",
             log::g(log::DEFAULT_LOG_EVERY));
    println!("      --logs DIR             where service and backfill write");
    println!("      --image FILE           backfill from a saved .bin, no counter");
    println!("      --serial SERIAL        which counter an image came from");
    println!("      --bytes N              how much flash to read");
    println!("      --max-gap SECONDS      a longer hole ends the averages");
    println!("      --poll SECONDS         hotplug: how often /dev is read (default 4)");
    println!("      --settle SECONDS       hotplug: grace before a new node is opened (default 2)");
    println!("      --tries N              hotplug: attempts per plug event (default 3)");
    println!("  -o, --output STEM          where log pull writes .bin and .csv");
    println!();
    println!("export, site and recompute are in the");
    println!("Python program in the same repository. They are being ported; the");
    println!("log format is here already, and tests/test_differential.py is what");
    println!("says it is the same format and not a second dialect of it.");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut command = String::new();
    let mut device: Option<String> = None;
    let mut baud: Option<u32> = None;
    let mut spans = vec![3.0, 30.0, 300.0, 3000.0, 30000.0];
    let mut cpm_per_usvh = counter::DEFAULT_CPM_PER_USVH;
    let mut duration: Option<f64> = None;
    let mut log_every = log::DEFAULT_LOG_EVERY;
    let mut logs: Option<std::path::PathBuf> = None;
    let mut check: Option<std::path::PathBuf> = None;
    let mut image: Option<std::path::PathBuf> = None;
    let mut bytes: Option<usize> = None;
    let mut max_gap = 300.0f64;
    let mut no_backfill = false;
    let mut set_clock = false;
    let mut output: Option<String> = None;
    // hotplug's three. Four seconds is a read of /dev fifteen times a minute,
    // which costs nothing; settle is the grace a node gets between appearing
    // and being opened, and tries is how many times one plug event is worth
    // retrying before it is written off.
    let mut poll: f64 = 4.0;
    let mut settle: f64 = 2.0;
    let mut tries: u32 = 3;
    let mut log_action = "info".to_string();
    let mut serial: Option<String> = None;

    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        let next = |i: &mut usize| -> Option<String> {
            *i += 1;
            args.get(*i).cloned()
        };
        match a {
            "-h" | "--help" => {
                usage();
                return;
            }
            "--version" => {
                println!("radbeeper {}", VERSION);
                return;
            }
            "-d" | "--device" => device = next(&mut i),
            "-b" | "--baud" => baud = next(&mut i).and_then(|v| v.parse().ok()),
            "--spans" => {
                spans = match next(&mut i).as_deref().and_then(parse_spans) {
                    Some(s) => s,
                    None => {
                        eprintln!("radbeeper: --spans wants whole seconds, comma separated");
                        std::process::exit(2);
                    }
                }
            }
            "--cpm-per-usvh" => {
                cpm_per_usvh = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(cpm_per_usvh)
            }
            "--duration" => duration = next(&mut i).and_then(|v| v.parse().ok()),
            "--logs" => logs = next(&mut i).map(std::path::PathBuf::from),
            "--log-every" => {
                log_every = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(log_every)
            }
            "--check" => check = next(&mut i).map(std::path::PathBuf::from),
            "--image" => image = next(&mut i).map(std::path::PathBuf::from),
            "--serial" => serial = next(&mut i),
            "--bytes" | "--backfill-bytes" => bytes = next(&mut i).and_then(|v| v.parse().ok()),
            "--no-backfill" => no_backfill = true,
            "--set" => set_clock = true,
            "--max-gap" => {
                max_gap = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(max_gap)
            }
            "-o" | "--output" => output = next(&mut i),
            "--poll" => poll = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(poll),
            "--settle" => settle = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(settle),
            "--tries" => tries = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(tries),
            "info" | "pull" if command == "log" => log_action = a.to_string(),
            "export" | "site" => {
                eprintln!(
                    "radbeeper: `{}` is not in the Rust build -- it writes the log\n\
                     format, which the Python program owns. Use that one:\n\
                     \n    https://github.com/vonglurt/radbeeper",
                    a
                );
                std::process::exit(2);
            }
            _ if a.starts_with('-') => {
                eprintln!("radbeeper: unknown option {}", a);
                std::process::exit(2);
            }
            _ => command = a.to_string(),
        }
        i += 1;
    }
    if command.is_empty() {
        usage();
        return;
    }

    if command == "backfill" {
        std::process::exit(backfill_cmd(
            &spans, log_every, max_gap, bytes.unwrap_or(64 * 1024),
            device.as_deref(), baud, logs, image.as_deref(),
            serial.as_deref(), output.as_deref(),
        ));
    }
    if command == "log" {
        std::process::exit(log_cmd(&log_action, bytes, output.as_deref(),
                                   device.as_deref(), baud));
    }
    if command == "random" {
        // --check needs no hardware, so it runs before anything looks for a
        // counter: an audit of a file from another machine is the ordinary
        // case, not an odd one.
        if let Some(p) = check {
            std::process::exit(check_random(&p));
        }
        std::process::exit(random(&spans, duration, device.as_deref(), baud, logs));
    }
    if command == "hotplug" {
        std::process::exit(hotplug(device.as_deref(), baud, poll, settle, tries, duration));
    }
    if command == "service" {
        let backfill = (!no_backfill).then(|| (bytes.unwrap_or(64 * 1024), max_gap));
        std::process::exit(service(
            &spans, log_every, duration, device.as_deref(), baud, logs, backfill,
        ));
    }

    let found = counter::find(device.as_deref(), baud);
    let c = match found {
        Ok(c) => c,
        Err(e) => {
            eprintln!("{}: {}", if e.busy { "port busy" } else { "no counter" }, e.reason);
            for line in e.detail.lines() {
                eprintln!("    {}", line);
            }
            std::process::exit(1);
        }
    };

    match command.as_str() {
        "probe" => {
            println!("counter    {}", c.version);
            println!("model      {}", c.model());
            println!("serial     {}", c.serial_no);
            println!("port       {} @ {} baud", c.path, c.baud);
            if let Some(v) = c.voltage() {
                println!("battery    {:.1} V", v);
            }
            if let Some(o) = c.clock_ahead() {
                println!("its clock  {}   {}",
                         clock::format(o.theirs, "%Y-%m-%d %H:%M:%S"), describe_offset(&o));
                if o.ahead.abs() > 1.0 {
                    println!("           radbeeper clock --set  corrects it from this machine");
                }
            }
            if let Some(n) = c.cpm() {
                println!(
                    "reading    {} CPM  ({:.3} uSv/h at {:.1} CPM per uSv/h)",
                    n, n as f64 / cpm_per_usvh, cpm_per_usvh
                );
            }
        }
        "cpm" => std::process::exit(cpm_cmd(&c, cpm_per_usvh)),
        "clock" => std::process::exit(clock_cmd(&c, set_clock)),
        "watch" => watch(&c, &spans, cpm_per_usvh, duration, logs),
        other => {
            eprintln!("radbeeper: unknown command {}", other);
            std::process::exit(2);
        }
    }
}

// ----------------------------------------------------------------- hotplug ---
//
// WHY THE SESSION WATCHES AND udev DOES NOT. A udev rule fires as root, in
// whatever environment udev happens to have: no WAYLAND_DISPLAY, no session
// bus, and no idea which of several logged-in people a window would belong to.
// Starting a background LOGGER from udev is right, and the rule Copal installs
// does exactly that. Opening a WINDOW from udev is guesswork. So the two
// halves are split at the line where the guessing starts: the rule starts the
// service, and this -- one process inside the session, on the desktop's
// autostart line -- opens the monitor.
//
// WHAT IT POLLS, AND WHAT IT DOES NOT. The names in /dev, never the serial
// port. See counter::candidate_ports.

/// Terminal emulators the autostart will open the monitor in, best first.
/// foot is the Wayland session's; the rest are what an X11 one is likely to
/// have. The first that exists wins.
const TERMINALS: [(&str, &str); 5] = [
    ("foot", "-e"),
    ("alacritty", "-e"),
    ("urxvt", "-e"),
    ("xterm", "-e"),
    ("st", "-e"),
];

/// The first terminal emulator on PATH, as (path, exec flag).
fn find_terminal() -> Option<(PathBuf, &'static str)> {
    find_terminal_in(&std::env::var_os("PATH")?)
}

/// The search itself, given the PATH to search.
///
/// Taking the value rather than reading the environment is what makes this
/// testable: a test can lay out a directory of its own and ask which terminal
/// would be picked, without setting a variable the whole process shares.
fn find_terminal_in(path: &std::ffi::OsStr) -> Option<(PathBuf, &'static str)> {
    // BEST FIRST, AND THE ORDER IS THE POINT: the whole of PATH is searched
    // for foot before anything is searched for alacritty. A session that has
    // both wants the one its desktop installed, not the one that happens to
    // sit in an earlier directory.
    for (term, flag) in TERMINALS {
        for dir in std::env::split_paths(path) {
            let p = dir.join(term);
            if p.is_file() && is_executable(&p) {
                return Some((p, flag));
            }
        }
    }
    None
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).map(|m| m.permissions().mode() & 0o111 != 0).unwrap_or(false)
}

/// When a window is wanted, with no `/dev` and no processes in it.
///
/// This is the whole of `hotplug` that can be wrong, so it is the whole of
/// `hotplug` that is tested. The loop around it does two things a test cannot
/// usefully do: read `/dev`, and start a terminal.
struct Watcher {
    tries: u32,
    max_tries: u32,
    settle: f64,
    due: f64,
}

impl Watcher {
    /// A counter already plugged in at login is the same event as one plugged
    /// in later, and gets the same retries rather than a special case. That is
    /// the whole reason the autostart line runs this and not a one-shot.
    fn new(max_tries: u32, settle: f64, anything_plugged_in: bool, now: f64) -> Watcher {
        Watcher {
            tries: if anything_plugged_in { max_tries } else { 0 },
            max_tries,
            settle,
            due: now,
        }
    }

    /// Should a window be opened at `now`? `window_open` is whether the last
    /// one is still up.
    fn should_open(&mut self, now: f64, window_open: bool) -> bool {
        if window_open {
            // It took: this plug event is dealt with, and no later tick is to
            // open a second window for it.
            self.tries = 0;
            return false;
        }
        if self.tries > 0 && now >= self.due {
            // THE RETRIES ARE FOR THE GAP between a node appearing and udev
            // giving it its group: the first open can be EACCES on a node that
            // is perfectly good a second later. A few attempts, then the event
            // is written off -- a cable that is not a counter must not be
            // opened again every four seconds for the rest of the session.
            self.tries -= 1;
            self.due = now + self.settle;
            return true;
        }
        false
    }

    /// Nodes that were not there before are there now.
    fn plugged(&mut self, now: f64) {
        self.tries = self.max_tries;
        self.due = now + self.settle;
    }
}

/// Open the monitor in a terminal, if there is something to watch.
///
/// DELIBERATELY SILENT WHEN THERE IS NO COUNTER. A window that opens at every
/// login to say "nothing is plugged in" gets closed at every login and then
/// gets deleted. A busy port is silent too: it means the counter is already
/// being read, by the service or by a monitor this session opened earlier, and
/// neither wants a second window -- the status file belongs to whoever holds
/// the port.
fn open_window(device: Option<&str>, baud: Option<u32>) -> Option<Child> {
    match counter::find(device, baud) {
        Ok(_) => {}
        Err(e) => {
            if !e.busy {
                log::write_status(&format!("dormant: {}", e.reason));
            }
            return None;
        }
    }
    let (term, flag) = match find_terminal() {
        Some(t) => t,
        None => {
            eprintln!("no terminal emulator found; run: radbeeper watch");
            return None;
        }
    };
    // AN ABSOLUTE PATH TO OURSELVES. The terminal inherits whatever directory
    // the caller had, and hotplug is started from a session whose directory is
    // not this checkout. A relative argv[0] works from the Makefile and
    // nowhere else, which is the worst way for it to be wrong.
    let me = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("radbeeper: cannot find my own path: {}", e);
            return None;
        }
    };
    let mut cmd = Command::new(&term);
    cmd.arg(flag).arg(&me);
    if let Some(d) = device {
        cmd.arg("--device").arg(d);
    }
    if let Some(b) = baud {
        cmd.arg("--baud").arg(b.to_string());
    }
    cmd.arg("watch");
    // setsid, so the window outlives the watcher that opened it.
    //
    // SAFETY: setsid is async-signal-safe and touches nothing this process
    // shares with the child between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    match cmd.spawn() {
        Ok(child) => Some(child),
        Err(e) => {
            eprintln!("radbeeper: could not open {}: {}", term.display(), e);
            None
        }
    }
}

/// `radbeeper hotplug`: open the monitor when a counter appears -- now, or in
/// an hour's time.
fn hotplug(
    device: Option<&str>,
    baud: Option<u32>,
    poll: f64,
    settle: f64,
    tries: u32,
    duration: Option<f64>,
) -> i32 {
    let mut seen: BTreeSet<String> = counter::candidate_ports().into_iter().collect();
    let started = Instant::now();
    let now = || started.elapsed().as_secs_f64();
    let mut w = Watcher::new(tries, settle, !seen.is_empty(), now());
    let mut child: Option<Child> = None;

    loop {
        // Is the last window still up? A window that was closed, or that never
        // opened at all, leaves this None.
        if let Some(c) = child.as_mut() {
            match c.try_wait() {
                Ok(Some(_)) | Err(_) => child = None,
                Ok(None) => {}
            }
        }
        if w.should_open(now(), child.is_some()) {
            child = open_window(device, baud);
        }
        if duration.map_or(false, |d| now() >= d) {
            return 0;
        }
        std::thread::sleep(Duration::from_secs_f64(poll));
        let ports: BTreeSet<String> = counter::candidate_ports().into_iter().collect();
        if ports.difference(&seen).next().is_some() {
            w.plugged(now());
        }
        seen = ports;
    }
}

// ----------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    /// The header the monitor draws on row 0, at its real length: a path, a
    /// baud rate, a firmware string and a fourteen-character serial.
    const HEAD: &str = "/dev/ttyUSB0 @ 115200 baud   GMC-320Re 4.26   serial F48824B8207F7E";

    /// A counter already plugged in at login gets a window, and gets it at
    /// once: the autostart line runs hotplug precisely so that "already there"
    /// and "plugged in later" are one case.
    #[test]
    fn a_counter_already_there_at_login_opens_a_window() {
        let mut w = Watcher::new(3, 2.0, true, 0.0);
        assert!(w.should_open(0.0, false), "no waiting for a plug that already happened");
    }

    #[test]
    fn an_empty_dev_opens_nothing_and_waits() {
        let mut w = Watcher::new(3, 2.0, false, 0.0);
        assert!(!w.should_open(0.0, false));
        assert!(!w.should_open(100.0, false), "and goes on not opening one");
        // Until something appears.
        w.plugged(100.0);
        assert!(!w.should_open(101.0, false), "the node gets its settling time");
        assert!(w.should_open(102.0, false));
    }

    /// THE RETRIES ARE FOR THE GAP between a node appearing and udev giving it
    /// its group: the first open can be EACCES on a node that is good a second
    /// later. Three attempts, then the event is written off -- a cable that is
    /// not a counter must not be opened again every four seconds all session.
    #[test]
    fn a_plug_event_is_retried_and_then_written_off() {
        let mut w = Watcher::new(3, 2.0, true, 0.0);
        let mut opens = 0;
        let mut t = 0.0;
        while t < 60.0 {
            if w.should_open(t, false) {
                opens += 1;
            }
            t += 1.0;
        }
        assert_eq!(opens, 3, "three tries for one plug event, and no more");
    }

    #[test]
    fn a_window_that_took_stops_the_retries() {
        let mut w = Watcher::new(3, 2.0, true, 0.0);
        assert!(w.should_open(0.0, false), "the first attempt");
        // It opened and is still up.
        assert!(!w.should_open(4.0, true));
        // And now it is closed again -- but the event was dealt with, so this
        // does not open a second window behind the person who closed the first.
        assert!(!w.should_open(8.0, false), "closing the window is not a plug event");
        assert!(!w.should_open(400.0, false));
    }

    #[test]
    fn a_second_counter_is_a_second_event() {
        let mut w = Watcher::new(3, 2.0, true, 0.0);
        assert!(w.should_open(0.0, false));
        assert!(!w.should_open(4.0, true), "the first window took");
        // Another node appears while that window is up.
        w.plugged(10.0);
        assert!(!w.should_open(11.0, false), "settling");
        assert!(w.should_open(12.0, false), "and the new one gets its window");
    }

    /// The terminals are tried in order, and the list is the one the desktop
    /// actually installs. foot is the Wayland session's.
    #[test]
    fn the_terminals_are_tried_best_first() {
        assert_eq!(TERMINALS[0].0, "foot");
        assert!(TERMINALS.iter().all(|(_, flag)| *flag == "-e"));
        assert!(TERMINALS.iter().any(|(t, _)| *t == "xterm"), "an X11 session has one of these");
    }

    fn a_terminal_called(dir: &Path, name: &str, executable: bool) {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(name);
        std::fs::write(&p, b"#!/bin/sh\n").unwrap();
        let mode = if executable { 0o755 } else { 0o644 };
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    #[test]
    fn the_terminal_search_prefers_the_desktops_over_the_directory_order() {
        let root = std::env::temp_dir().join(format!("radbeeper-term-{}", std::process::id()));
        let (first, second) = (root.join("first"), root.join("second"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        // xterm sits in the EARLIER directory and foot in the later one. foot
        // still wins: the list is ordered by what the desktop installs, not by
        // what PATH happens to reach first.
        a_terminal_called(&first, "xterm", true);
        a_terminal_called(&second, "foot", true);
        let path = std::env::join_paths([&first, &second]).unwrap();
        let (found, flag) = find_terminal_in(&path).expect("one of them");
        assert_eq!(found, second.join("foot"));
        assert_eq!(flag, "-e");

        // A file that is not executable is not a terminal.
        let root2 = root.join("third");
        std::fs::create_dir_all(&root2).unwrap();
        a_terminal_called(&root2, "foot", false);
        a_terminal_called(&root2, "st", true);
        let path = std::env::join_paths([&root2]).unwrap();
        assert_eq!(find_terminal_in(&path).unwrap().0, root2.join("st"));

        // And a PATH with none of them opens nothing rather than guessing.
        let empty = root.join("empty");
        std::fs::create_dir_all(&empty).unwrap();
        let path = std::env::join_paths([&empty]).unwrap();
        assert!(find_terminal_in(&path).is_none());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_big_digits_start_clear_of_the_header() {
        // THE REGRESSION. This was a constant 54 while the header was 67
        // columns wide, so the readout was drawn straight through the
        // counter's own serial number -- at every terminal width, on both
        // implementations, and nowhere near where anyone was looking for it.
        assert!(HEAD.chars().count() > 54, "the header outgrew the old constant");
        assert!(digits_left(HEAD) > HEAD.chars().count());
    }

    #[test]
    fn a_shorter_header_lets_the_digits_come_left() {
        // Measured, not a new constant: a counter on a short path with a
        // short serial should not push the number needlessly right.
        assert!(digits_left("/dev/ttyUSB0 @ 57600 baud   GMC-300  1.0   serial A1")
                < digits_left(HEAD));
    }

    #[test]
    fn every_character_the_readout_can_contain_has_a_glyph() {
        // The readout is a formatted f64, or "--" before a window is full.
        for ch in "0123456789.-".chars() {
            assert!(glyph(ch).is_some(), "no glyph for {:?}", ch);
        }
        assert!(glyph('x').is_none());
    }

    #[test]
    fn a_big_number_is_twelve_rows_of_equal_width() {
        let rows = big_number("40.5");
        assert_eq!(rows.len(), BIG_ROWS);
        let w = rows[0].chars().count();
        assert!(w > 0);
        for r in &rows {
            assert_eq!(r.chars().count(), w, "ragged row: {:?}", r);
        }
    }

    #[test]
    fn a_number_with_no_reading_yet_still_draws() {
        let rows = big_number("--");
        assert_eq!(rows.len(), BIG_ROWS);
        assert!(rows.iter().any(|r| r.contains('█')));
    }

    #[test]
    fn an_unknown_character_is_skipped_rather_than_drawn_ragged() {
        // Whatever happens, the twelve rows stay the same width as each
        // other -- a ragged block is worse than a missing character.
        let rows = big_number("4x0");
        let w = rows[0].chars().count();
        assert!(rows.iter().all(|r| r.chars().count() == w));
    }

    #[test]
    fn spans_are_a_comma_list_and_must_be_positive() {
        assert_eq!(parse_spans("3,30,300,3000"),
                   Some(vec![3.0, 30.0, 300.0, 3000.0]));
        assert_eq!(parse_spans(" 1 , 10 "), Some(vec![1.0, 10.0]));
        assert_eq!(parse_spans("3,0"), None, "a zero-second window is not one");
        assert_eq!(parse_spans("3,-1"), None);
        assert_eq!(parse_spans("3,x"), None);
        assert_eq!(parse_spans(""), None);
    }

    #[test]
    fn a_cursor_move_is_one_based_on_the_wire_and_zero_based_here() {
        // at(0, 0) has to be the top left corner, not one row down from it.
        assert_eq!(at(0, 0), "\x1b[1;1H");
        assert_eq!(at(11, 53), "\x1b[12;54H");
    }

    #[test]
    fn the_bands_colour_the_number_the_same_way_they_colour_the_bars() {
        assert_ne!(colour_for(Level::Calm), colour_for(Level::Raised));
        assert_ne!(colour_for(Level::Raised), colour_for(Level::High));
    }
}
