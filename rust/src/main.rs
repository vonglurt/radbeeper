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

use radbeeper::{analysis, clock, log};
use analysis::{
    bar_rows, level, spectrum_columns, Ladder, Level, Windows,
};
use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, SystemTime};

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
fn watch(c: &counter::Counter, spans: &[f64], cpm_per_usvh: f64, duration: Option<f64>) {
    let screen = Screen::enter();
    let mut w = Windows::new(spans);
    let mut ladder = Ladder::new();
    c.heartbeat(true);
    let start = Instant::now();
    let mut out = String::with_capacity(16384);

    loop {
        let counts = match c.next_sample(Duration::from_millis(2500)) {
            Some(v) => v as u32,
            None => break,
        };
        let when = start.elapsed().as_secs_f64();
        w.add(when, counts);
        ladder.add(counts);
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
                        "{}{}{:>4}s   filling, {}s to go{}",
                        at(row, 0), DIM, span as i64, left, OFF
                    ));
                }
                Some(cpm) => {
                    out.push_str(&format!(
                        "{}{}{:>4}s{}{}{}{:>8.1} CPM{}{}{:>8.3} uSv/h",
                        at(row, 0), DIM, span as i64, OFF,
                        at(row, 6), colour_for(level(cpm)), cpm, OFF,
                        at(row, 22), cpm / cpm_per_usvh
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
        row += 2;

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
           logs: Option<std::path::PathBuf>) -> i32 {
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
    println!("radbeeper: backfill is not in this build yet -- \
              the Python does that one");

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
    println!("  radbeeper watch            the monitor");
    println!("  radbeeper service          log to disk, a row every 30 seconds");
    println!();
    println!("  -d, --device PATH          serial port (default: search /dev)");
    println!("  -b, --baud RATE            baud (default: try 115200 then 57600)");
    println!("      --spans 3,30,300,3000  averaging windows, seconds");
    println!("      --cpm-per-usvh N       tube factor (default {})", counter::DEFAULT_CPM_PER_USVH);
    println!("      --duration SECONDS     stop after this long");
    println!("      --log-every SECONDS    row spacing for service (default {})",
             log::g(log::DEFAULT_LOG_EVERY));
    println!("      --logs DIR             where service writes (default: the");
    println!("                             state directory)");
    println!();
    println!("backfill, export, site, random, recompute and hotplug are in the");
    println!("Python program in the same repository. They are being ported; the");
    println!("log format is here already, and tests/test_differential.py is what");
    println!("says it is the same format and not a second dialect of it.");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut command = String::new();
    let mut device: Option<String> = None;
    let mut baud: Option<u32> = None;
    let mut spans = vec![3.0, 30.0, 300.0, 3000.0];
    let mut cpm_per_usvh = counter::DEFAULT_CPM_PER_USVH;
    let mut duration: Option<f64> = None;
    let mut log_every = log::DEFAULT_LOG_EVERY;
    let mut logs: Option<std::path::PathBuf> = None;

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
            "backfill" | "export" | "site" | "random" | "hotplug" | "log" => {
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

    if command == "service" {
        std::process::exit(service(
            &spans, log_every, duration, device.as_deref(), baud, logs,
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
            if let Some(d) = c.datetime() {
                let now = SystemTime::now();
                let secs = now.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                println!("its clock  {}   (this machine: unix {})", d, secs);
            }
            if let Some(n) = c.cpm() {
                println!(
                    "reading    {} CPM  ({:.3} uSv/h at {:.1} CPM per uSv/h)",
                    n, n as f64 / cpm_per_usvh, cpm_per_usvh
                );
            }
        }
        "cpm" => match c.cpm() {
            Some(n) => println!("{} CPM   {:.3} uSv/h", n, n as f64 / cpm_per_usvh),
            None => {
                eprintln!("the counter did not answer");
                std::process::exit(1);
            }
        },
        "watch" => watch(&c, &spans, cpm_per_usvh, duration),
        other => {
            eprintln!("radbeeper: unknown command {}", other);
            std::process::exit(2);
        }
    }
}

// ----------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    /// The header the monitor draws on row 0, at its real length: a path, a
    /// baud rate, a firmware string and a fourteen-character serial.
    const HEAD: &str = "/dev/ttyUSB0 @ 115200 baud   GMC-320Re 4.26   serial F48824B8207F7E";

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
