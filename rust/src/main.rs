// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
// radbeeper -- a GQ GMC Geiger-Muller counter on the desk. The read side,
// native: find it, ask what it is, and watch it.
//
// Everything that writes the log format -- service, backfill, export, site,
// random, hotplug -- is in the one-file Python program in the same repository
// and stays there while that format is still moving. Asking this binary for
// one of them says so rather than pretending.
mod analysis;
mod counter;
mod serial;

use analysis::{
    bar_rows, level, spectrum_columns, Ladder, Level, Windows,
};
use std::io::{Read, Write};
use std::time::{Duration, Instant, SystemTime};

const VERSION: &str = env!("CARGO_PKG_VERSION");
const BIG_ROWS: usize = 12;

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
        out.push_str(&format!(
            "{}{}{} @ {} baud   {}   serial {}{}",
            at(0, 0), DIM, c.path, c.baud, c.version, c.serial_no, OFF
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
        if width > 54 + wide + 6 && h > BIG_ROWS {
            let tint = headline.map(|v| colour_for(level(v))).unwrap_or(DIM);
            for (i, d) in digits.iter().enumerate() {
                out.push_str(&format!("{}{}{}{}", at(i, 54), tint, d, OFF));
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
    println!();
    println!("  -d, --device PATH          serial port (default: search /dev)");
    println!("  -b, --baud RATE            baud (default: try 115200 then 57600)");
    println!("      --spans 3,30,300,3000  averaging windows, seconds");
    println!("      --cpm-per-usvh N       tube factor (default {})", counter::DEFAULT_CPM_PER_USVH);
    println!("      --duration SECONDS     stop after this long");
    println!();
    println!("service, backfill, export, site, random and hotplug are in the");
    println!("Python program in the same repository, which owns the log format.");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut command = String::new();
    let mut device: Option<String> = None;
    let mut baud: Option<u32> = None;
    let mut spans = vec![3.0, 30.0, 300.0, 3000.0];
    let mut cpm_per_usvh = counter::DEFAULT_CPM_PER_USVH;
    let mut duration: Option<f64> = None;

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
            "service" | "backfill" | "export" | "site" | "random" | "hotplug" | "log" => {
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
