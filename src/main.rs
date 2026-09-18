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

use radbeeper::{analysis, broker, clock, entropy, history, log};
use analysis::{
    bar_rows, bar_rows_to, level, spectrum_columns, Ladder, Level, Windows,
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
/// Samples kept for the cascade strip. Four tiers of doubling reach back
/// about a quarter of an hour at any width a terminal has; the rest is the
/// windows' business, not the strip's.
const STRIP_KEEP: usize = 4096;
/// How often `--wait` looks at a busy port again.
///
/// Half a second, where the service takes ten. The service is waiting for a
/// person to finish watching and can afford to be slow about it; `--wait` is a
/// person at a second window who has just run the command that frees the port,
/// and a wait that visibly lags the handover reads as a wait that did not
/// work. Polling this fast is safe because an open that fails at the flock
/// never reaches termios and never writes a byte -- see serial::Serial::open,
/// where the lock is taken before configure -- so the holder cannot tell.
const WAIT_POLL: f64 = 0.5;

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

/// The widest reading there is. `<GETCPM>>` answers in two bytes, so 65535
/// is the most this counter can ever say, and the big readout keeps room for
/// all five digits whether or not this second needs them.
const MAX_READOUT: &str = "65535";

/// How many columns the readout is allowed, and always takes.
fn readout_width() -> usize {
    big_number(MAX_READOUT)
        .iter()
        .map(|r| r.chars().count())
        .max()
        .unwrap_or(0)
}

/// What is drawing this screen, for a screen that outlives its terminal: a
/// recording, a screenshot pasted into somebody's issue. Out of the manifest,
/// so it cannot drift from the binary.
fn nameplate() -> String {
    format!("{} {}", env!("CARGO_PKG_NAME"), env!("CARGO_PKG_VERSION"))
}

/// Where the nameplate goes: hard against the right edge of the top row, in
/// the corner the readout does not reach -- and nowhere at all if it would
/// come within two columns of `busy_to`. The counter has priority; a version
/// string is worth nothing beside a number somebody is watching.
fn nameplate_left(width: usize, busy_to: usize, plate: usize) -> Option<usize> {
    let x = width.checked_sub(plate)?;
    (x > busy_to + 1).then_some(x)
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

/// What the monitor writes while it is open. `--no-log` is None, and then it
/// writes nothing at all.
struct WatchLog {
    dir: PathBuf,
    every: f64,
    backfill: Option<(usize, f64)>,
    /// index.html and random.html into `dir`, and how often while open.
    export_every: Option<f64>,
}

/// Write the pages into the log directory, and say so in a few words.
///
/// WHEN, NOT HOW OFTEN. After the backfill, so the page carries the gap it
/// just filled; every `export_every` while open, so a page somebody has up
/// is at most that stale; and on quit, so the last thing the monitor saw is
/// on it. Not on every row: a month of log is tens of thousands of rows and
/// the page is rebuilt from all of them.
fn export_pages(dir: &Path, cpm_per_usvh: f64) -> String {
    match radbeeper::export::export(dir, &dir.join("index.html"), true, cpm_per_usvh,
                                    radbeeper::export::DEFAULT_TITLE, None) {
        Ok(r) => format!("exported {} rows {}", r.rows,
                         clock::format(clock::now(), "%H:%M")),
        Err(e) => format!("NOT EXPORTED: {}", e),
    }
}

/// How many rows of log the table at the bottom gets on a screen `h` tall.
///
/// The charts come first. Below 36 rows they are drawn compact -- the counts
/// in three rows, the spectrum in one -- and everything under 24 rows goes to
/// the table; from 36 the charts are full size again and the table keeps six
/// rows, taking every row after that.
fn table_lines(h: usize) -> usize {
    if h < 26 {
        0
    } else if h < 36 {
        h - 24
    } else {
        h - 30
    }
}

/// The table: the log's own header and its newest rows, aligned in columns
/// as wide as their widest cell and cut at the screen's edge, oldest at the
/// top so each new row pushes the rest up like a terminal scrolling.
///
/// The same cells the log is written with, so what is on screen is what is on
/// disk. Rows the flash filled in are dim; a field with no value is a dim
/// `-`, because a window that was not full yet is not a zero.
fn draw_table(out: &mut String, top: usize, lines: usize, width: usize,
              names: &[String], rows: &std::collections::VecDeque<Vec<String>>) {
    if lines == 0 {
        return;
    }
    let shown: Vec<&Vec<String>> = rows.iter().rev().take(lines - 1).rev().collect();
    let src = names.iter().position(|n| n == "src");
    let mut widths: Vec<usize> = names.iter().map(|n| n.len() + 1).collect();
    for r in &shown {
        for (i, cell) in r.iter().enumerate() {
            if let Some(wd) = widths.get_mut(i) {
                *wd = (*wd).max(cell.chars().count().max(1));
            }
        }
    }
    // As many columns as fit, two spaces apart, never wrapping.
    let mut fit = 0;
    let mut used = 0;
    for wd in &widths {
        if used + wd > width.saturating_sub(1) {
            break;
        }
        used += wd + 2;
        fit += 1;
    }
    let mut head = String::new();
    for (i, name) in names.iter().take(fit).enumerate() {
        let label = if i == 0 { format!("#{}", name) } else { name.clone() };
        head.push_str(&format!("{:<w$}  ", label, w = widths[i]));
    }
    let head = head.trim_end().to_string();
    out.push_str(&format!("{}{}{}{}", at(top, 0), DIM, head, OFF));
    for (k, r) in shown.iter().enumerate() {
        let flash = src.and_then(|i| r.get(i)).map(|v| v != log::SRC_LIVE).unwrap_or(false);
        out.push_str(&at(top + 1 + k, 0));
        if flash {
            out.push_str(DIM);
        }
        for i in 0..fit {
            let cell = r.get(i).map(String::as_str).unwrap_or("");
            if cell.is_empty() {
                out.push_str(&format!("{}{:<w$}{}  ", DIM, "-", if flash { "" } else { OFF },
                                      w = widths[i]));
            } else {
                out.push_str(&format!("{:<w$}  ", cell, w = widths[i]));
            }
        }
        out.push_str(OFF);
    }
}

/// Several counters, read at once.
///
/// A THREAD EACH, AND A CHANNEL OUT. Two tubes are two file descriptors that
/// go quiet independently, and reading them in turn on one thread means a
/// counter that has been unplugged costs the other one a 2.5-second timeout
/// every second. A thread per counter costs a few kilobytes of stack and
/// makes the quiet case free; the channel puts the samples back in arrival
/// order, which is exactly the order the display wants them in.
///
/// WHY ARRIVAL ORDER IS THE INTERESTING ORDER. Two counters do not agree on
/// when a second starts -- each has its own clock and its own phase -- so
/// their samples interleave. That interleaving is the whole benefit: the same
/// tube watching the same room twice a second, at some offset, is a finer grid
/// in time than either counter can produce alone.
struct Bank {
    counters: Vec<std::sync::Arc<counter::Counter>>,
    rx: std::sync::mpsc::Receiver<(usize, f64, u32)>,
    /// Held until `start`, and dropped by it. While it exists the channel
    /// cannot report that every reader has finished, which is what stops a
    /// bank that has not been started yet from looking like one that is over.
    tx: Option<std::sync::mpsc::Sender<(usize, f64, u32)>>,
}

impl Bank {
    /// Take the counters. DOES NOT START READING -- see `start`.
    fn open(found: Vec<counter::Counter>) -> Bank {
        let (tx, rx) = std::sync::mpsc::channel();
        Bank {
            counters: found.into_iter().map(std::sync::Arc::new).collect(),
            rx,
            tx: Some(tx),
        }
    }

    /// Turn the stream on and start reading it.
    ///
    /// SEPARATE FROM `open`, AND THE SEPARATION IS LOAD-BEARING. Before the
    /// samples start there is a conversation to have with each counter --
    /// reading the tail of its flash to fill the log's gaps, which is a
    /// request and a reply over the same file descriptor. A reader thread
    /// running during that conversation eats the flash as though it were
    /// counts and hands the backfill the counts as though they were flash:
    /// the monitor showed `16383 counts this second` -- which is the count
    /// mask, every bit set -- and a run of a quarter of a million counts in
    /// one second. Nothing about it looked like a race; it looked like a
    /// broken counter. So the port conversations happen first and this is
    /// called when they are done.
    fn start(&mut self) {
        let Some(tx) = self.tx.take() else {
            return;
        };
        for (i, c) in self.counters.iter().enumerate() {
            // THE COUNTER HAS TO BE ASKED TO TALK, and each one separately.
            c.heartbeat(true);
            let c = c.clone();
            let tx = tx.clone();
            std::thread::spawn(move || loop {
                match c.next_sample(Duration::from_millis(2500)) {
                    Some(v) => {
                        if tx.send((i, clock::now(), v as u32)).is_err() {
                            return;
                        }
                    }
                    // This counter has gone quiet. Its thread ends; the
                    // others carry on, and the bank is only finished when
                    // every sender has been dropped.
                    None => return,
                }
            });
        }
    }

    fn identity(&self, spans: &[f64]) -> broker::Identity {
        broker::Identity {
            counters: self
                .counters
                .iter()
                .map(|c| broker::CounterId {
                    path: c.path.clone(),
                    baud: c.baud,
                    version: c.version.clone(),
                    serial_no: c.serial_no.clone(),
                })
                .collect(),
            spans: spans.to_vec(),
        }
    }

    fn len(&self) -> usize {
        self.counters.len()
    }

    fn next(&self, timeout: Duration) -> Option<(usize, f64, u32)> {
        self.rx.recv_timeout(timeout).ok()
    }

    fn stop(&self) {
        for c in &self.counters {
            c.heartbeat(false);
        }
    }
}

/// The mean rate across every tube, over `span` seconds.
///
/// TWO COUNTERS SEE TWICE THE COUNTS AND NOT TWICE THE DOSE. The combined
/// windows hold every sample from every tube, so their sum is `tubes` times
/// the rate the room is actually at; dividing gives the mean of the tubes,
/// which is the same number one tube would report and is measured from twice
/// as many arrivals. That is the entire bargain: identical accuracy, better
/// precision, in proportion to the square root of the counts behind it.
fn mean_cpm(w: &Windows, span: f64, tubes: usize) -> Option<f64> {
    w.average(span).map(|v| v / tubes.max(1) as f64)
}

/// Where a monitor's samples come from.
///
/// TWO WAYS TO HAVE A COUNTER, AND THE DIFFERENCE IS ONE flock. `Own` holds
/// the port, and therefore owes everybody else a fan-out: it logs, it draws
/// the entropy, it writes the emissions, and it publishes all three. `Attached`
/// holds nothing, opens nothing and writes nothing -- it is handed the same
/// samples a moment later and draws exactly the same screen from them.
///
/// The drawing code cannot tell which of the two it has, and that is the test
/// of whether this abstraction is in the right place. Everything below
/// next_sample() is arithmetic; the port is not part of it.
enum Feed {
    Own {
        bank: Bank,
        /// None when the socket could not be bound -- an unwritable log
        /// directory, or another server already there. The monitor still
        /// draws; it just cannot be attached to, and says so.
        srv: Option<broker::Server>,
    },
    Attached(broker::Client),
}

/// One thing that happened, whichever end it came from.
enum Tick {
    Sample { who: usize, when: f64, counts: u32 },
    /// A row the server wrote. WHICH counter it came from is carried on the
    /// wire and dropped here: this monitor draws one table and the rows are
    /// already stamped with their own serial. The window reads the tag.
    Row { row: String },
    Random { who: usize, hex: String, at: String, suspect: bool },
    /// The replayed history is over. Only an attached feed sends it.
    Live,
}

impl Feed {
    /// Attach to whoever holds the port, or take it.
    ///
    /// THE ORDER IS THE WHOLE FIX. Ask the socket first: if a service is
    /// logging, it is already reading the counter and there is nothing to
    /// negotiate -- attaching costs it one write a second and costs the
    /// person nothing at all. Only when nobody is serving does this open the
    /// port, and then it serves in turn, so the next window attaches to this
    /// one instead of printing `port busy`.
    fn open(dir: &Path, devices: &[String], baud: Option<u32>, spans: &[f64],
            wait: Option<f64>) -> Result<Feed, counter::NotFound> {
        // A NAMED DEVICE MEANS THAT DEVICE, AND THE SOCKET DOES NOT OVERRULE
        // IT. `-d /dev/pts/7` is how the fake counter is driven and how a
        // second counter is picked out on a machine with two; attaching to
        // whatever a service happens to be serving instead would answer a
        // question nobody asked. The differential suite caught this the hour
        // it was written: every `-d <pty> watch` in it silently drew the REAL
        // counter, because a service was running and the socket was tried
        // first. So attach only when the stream carries every counter that
        // was asked for -- which it does, when nothing was asked for.
        if let Some(c) = broker::Client::attach(dir) {
            if devices.iter().all(|d| c.identity().serves(d)) {
                return Ok(Feed::Attached(c));
            }
        }
        let one = devices.len() == 1;
        let found = match (wait, one) {
            (Some(limit), _) => vec![find_waiting(devices.first().map(|s| s.as_str()),
                                                  baud, limit)?],
            _ => {
                let (found, why) = counter::find_all(devices, baud);
                match why {
                    Some(e) => return Err(e),
                    None => found,
                }
            }
        };
        let bank = Bank::open(found);
        let srv = broker::Server::start(dir, &bank.identity(spans)).ok();
        Ok(Feed::Own { bank, srv })
    }

    fn identity(&self) -> broker::Identity {
        match self {
            // Spans are left empty for an owned feed: the monitor already has
            // its own and uses them, and the table it draws is its own file.
            Feed::Own { bank, .. } => bank.identity(&[]),
            Feed::Attached(c) => c.identity().clone(),
        }
    }

    /// The ports, for the things that genuinely need to talk to a counter --
    /// reading its flash, setting its clock. An attached feed has none, and
    /// the caller must have something sensible to do about that.
    fn counters(&self) -> &[std::sync::Arc<counter::Counter>] {
        match self {
            Feed::Own { bank, .. } => &bank.counters,
            Feed::Attached(_) => &[],
        }
    }

    fn owns_the_port(&self) -> bool {
        matches!(self, Feed::Own { .. })
    }

    /// How many other windows are reading this one's stream.
    fn watchers(&self) -> usize {
        match self {
            Feed::Own { srv, .. } => srv.as_ref().map(|s| s.clients()).unwrap_or(0),
            Feed::Attached(_) => 0,
        }
    }

    /// Begin reading. Nothing arrives before this, and it must not be called
    /// until every port conversation -- the backfill above all -- is over.
    fn start(&mut self) {
        if let Feed::Own { bank, .. } = self {
            bank.start();
        }
    }

    /// How many counters are behind this feed.
    fn width(&self) -> usize {
        match self {
            Feed::Own { bank, .. } => bank.len(),
            Feed::Attached(c) => c.identity().len(),
        }
    }

    /// Stop the stream -- but ONLY if this end started it. An attached
    /// monitor closing must not silence the counter for the service that is
    /// still logging it, which is the one way a client could do real damage.
    fn stop(&mut self) {
        if let Feed::Own { bank, .. } = self {
            bank.stop();
        }
    }

    fn next(&mut self, timeout: Duration) -> Option<Tick> {
        match self {
            Feed::Own { bank, srv } => {
                // Let in whoever turned up while we were waiting for this
                // second. Before the publish, so a client that has just been
                // greeted with the history gets this sample live rather than
                // twice.
                if let Some(s) = srv.as_mut() {
                    s.accept_pending();
                }
                // WALL CLOCK, NOT A MONOTONIC STAMP. A sample that is going
                // to be replayed to a process which was not running when it
                // was taken has to mean something to it, and "412.7 seconds
                // after some other program started" does not. It is also the
                // only clock two counters can be placed on together.
                let (who, when, counts) = bank.next(timeout)?;
                if let Some(s) = srv.as_mut() {
                    s.publish_sample(who, when, counts);
                }
                Some(Tick::Sample { who, when, counts })
            }
            Feed::Attached(c) => match c.next(timeout)? {
                broker::Event::Sample { who, when, counts } => {
                    Some(Tick::Sample { who, when, counts })
                }
                broker::Event::Row { row, .. } => Some(Tick::Row { row }),
                broker::Event::Random { who, hex, at, suspect } => {
                    Some(Tick::Random { who, hex, at, suspect })
                }
                broker::Event::Live => Some(Tick::Live),
            },
        }
    }

    fn publish_row(&mut self, who: usize, row: &str) {
        if let Feed::Own { srv: Some(s), .. } = self {
            s.publish_row(who, row);
        }
    }

    fn publish_random(&mut self, who: usize, hex: &str, at: &str, suspect: bool) {
        if let Feed::Own { srv: Some(s), .. } = self {
            s.publish_random(who, hex, at, suspect);
        }
    }
}

fn watch(feed: &mut Feed, spans: &[f64], cpm_per_usvh: f64,
         duration: Option<f64>, logging: Option<WatchLog>) {
    // So a `kill` stops the stream and puts the terminal back, as q does.
    install_stop_handler();
    let id = feed.identity();
    // THE TABLE IS SOMEBODY ELSE'S FILE, SO IT IS DRAWN WITH THEIR COLUMNS.
    // An attached monitor may have been given different --spans on the
    // command line, and its own averages panel honours them -- that is
    // arithmetic over samples it holds, and needs nobody's permission. The
    // rows at the bottom came off the server's disk and their columns are not
    // ours to choose. An owned feed leaves spans empty and uses its own.
    let table_spans: Vec<f64> =
        if id.spans.is_empty() { spans.to_vec() } else { id.spans.clone() };
    let screen = Screen::enter();
    let names = log::columns(&log::header(&table_spans));
    let mut table: std::collections::VecDeque<Vec<String>> = std::collections::VecDeque::new();
    let mut table_note = String::new();
    // A CLIENT WRITES NOTHING, AND THE RULE HAS NO EXCEPTIONS. The process
    // holding the port is already logging, backfilling and exporting this
    // counter; a second writer would race it for the same rows in the same
    // file, and the flock that stops two readers halving a count does nothing
    // at all about two writers interleaving a line. The rows still appear in
    // the table below -- they come down the socket as the server writes them,
    // which is a truer picture than this process's own guess would be.
    // WHETHER A TABLE WAS ASKED FOR, decided before the writing is taken
    // away. An attached monitor still shows the rows -- they arrive down the
    // socket as the server writes them -- and `--no-log` still means a clean
    // screen. Keying the table off `logger.is_some()` instead left an
    // attached window collecting rows it never drew.
    let wants_table = logging.is_some();
    let logging = logging.filter(|_| feed.owns_the_port());
    let mut loggers: Vec<Logger> = Vec::new();
    if let Some(wl) = logging.as_ref() {
        let _ = std::fs::create_dir_all(&wl.dir);
        // THE HISTORY FIRST, and it takes a while -- fifteen or twenty seconds
        // of flash over the serial line -- so the screen says what it is
        // waiting for instead of sitting blank.
        // EACH COUNTER'S OWN FLASH, EACH COUNTER'S OWN LOG. They are separate
        // instruments that were in the room at the same time; merging their
        // records would throw away the one thing a second tube is for, which
        // is being able to ask whether the two agree.
        for (k, c) in feed.counters().iter().enumerate() {
            if let Some((bytes, max_gap)) = wl.backfill {
                print!("\x1b[2J{}{}{} @ {} baud   {}   serial {}{}{}reading the counter's history \
                        to fill the log's gaps ({} KiB)...{}",
                       at(0, 0), DIM, c.path, c.baud, c.version, c.serial_no, OFF,
                       at(2, 0), bytes / 1024, at(3, 0));
                let _ = std::io::stdout().flush();
                let note = backfill_at_start(c, &wl.dir, spans, wl.every, bytes, max_gap, true)
                    .replace(" samples, ", " samples ")
                    .replace("backfill -- ", "backfill: ");
                table_note = if k == 0 { note } else { format!("{}   {}", table_note, note) };
            }
            // The table opens on what is already in the log, so it is not
            // empty for the first half-minute -- and what the backfill just
            // wrote is the first thing in it.
            let path = log::path(clock::now(), &wl.dir, Some(&c.serial_no));
            for (_, cells) in
                log::read_table(&path, &names).into_iter().rev().take(TABLE_KEEP).rev()
            {
                table.push_back(cells);
            }
            loggers.push(Logger::new(spans, wl.dir.clone(), &c.serial_no, wl.every));
        }
        if wl.export_every.is_some() {
            let done = export_pages(&wl.dir, cpm_per_usvh);
            table_note = if table_note.is_empty() { done } else { format!("{}   {}", table_note, done) };
        }
    }
    let mut exported = Instant::now();
    // HOW MANY TUBES ARE WATCHING THE SAME ROOM. Every combined figure below
    // is divided by it, because two counters see twice the counts and NOT
    // twice the dose -- what doubles is the evidence, not the radiation. The
    // precision that buys is the whole reason for a second tube, and it is
    // reported beside the number rather than left to be inferred.
    let tubes = feed.width().max(1);
    // One per counter, and one across all of them. The combined windows hold
    // every sample from every tube, so their sums are `tubes` times the rate:
    // see `mean_cpm`.
    let mut each: Vec<Windows> = (0..tubes).map(|_| Windows::new(spans)).collect();
    let mut w = Windows::new(spans);
    let mut ladder = Ladder::new();
    let mut pools: Vec<entropy::Entropy> =
        (0..tubes).map(|_| entropy::Entropy::default()).collect();
    // The samples in ARRIVAL order, as rates, which is what the cascade is
    // cut from. With one counter that is one bar a second; with two it is a
    // bar every half second on average, because two tubes on their own clocks
    // interleave. See analysis::tiers_with.
    let mut merged: std::collections::VecDeque<f64> = std::collections::VecDeque::new();
    let mut dropped = 0usize;
    // Whole seconds, summed across the tubes, for the spectrum: a period is a
    // property of the room and both counters are looking at it, so adding
    // them is simply twice the signal on one time base.
    let mut sec_bin: Option<i64> = None;
    let mut sec_sum: u32 = 0;
    // The drawn line and the moment it was drawn, kept across frames: a line
    // stays on screen until the pool has earned the next one.
    let mut shown: Option<(String, String)> = None;
    let mut suspect = false;
    // An attached feed opens with everything the server has: up to eight
    // hours of it, as fast as the socket can carry it. Those samples go into
    // the windows and the spectrum but are NOT drawn one frame each -- eight
    // hours of history, drawn a second at a time, takes eight hours.
    let mut replaying = !feed.owns_the_port();
    if replaying {
        table_note = match id.primary() {
            Some(c) if tubes == 1 => format!("attached to the counter on {}", c.path),
            _ => format!("attached to {} counters", tubes),
        };
    }
    // AFTER THE BACKFILL, NEVER BEFORE IT. See Bank::start.
    feed.start();
    let watching = Instant::now();
    let mut out = String::with_capacity(16384);
    // Rows written this second, held only until the logger's borrow ends.
    let mut rows: Vec<(usize, String)> = Vec::new();

    loop {
        let (who, when, counts) = match feed.next(Duration::from_millis(2500)) {
            Some(Tick::Sample { who, when, counts }) => (who, when, counts),
            // A row the server has just written. The client shows what went
            // to disk rather than deciding for itself what should have.
            Some(Tick::Row { row }) => {
                table.push_back(row.split('\t').map(str::to_string).collect());
                while table.len() > TABLE_KEEP {
                    table.pop_front();
                }
                continue;
            }
            // The hex on a client's screen is always the server's. Two
            // processes deriving their own from the same counts would print
            // two different lines and only one of them would be in the audit
            // log, which is worse than useless.
            Some(Tick::Random { who, hex, at, suspect: s }) => {
                shown = Some((hex, at));
                suspect = s;
                if let Some(p) = pools.get_mut(who) {
                    p.reset();
                }
                continue;
            }
            Some(Tick::Live) => {
                replaying = false;
                continue;
            }
            None => break,
        };
        if stopping() {
            break;
        }
        let who = who.min(tubes - 1);
        w.add(when, counts);
        each[who].add(when, counts);
        if merged.len() >= STRIP_KEEP {
            merged.pop_front();
            dropped += 1;
        }
        merged.push_back(counts as f64);
        // The spectrum's second closes when the wall clock's does, and takes
        // whatever every tube put in it.
        let this = when.floor() as i64;
        match sec_bin {
            Some(b) if b == this => sec_sum += counts,
            Some(_) => {
                ladder.add(sec_sum);
                sec_bin = Some(this);
                sec_sum = counts;
            }
            None => {
                sec_bin = Some(this);
                sec_sum = counts;
            }
        }
        // NOT DURING THE REPLAY. The server's pool holds the counts since its
        // last draw; a client that poured eight hours of history into its own
        // would think it had earned a line the moment it opened. Starting at
        // the live edge makes the countdown pessimistic until the first
        // emission arrives, and exact from then on.
        if !replaying {
            if let Some(p) = pools.get_mut(who) {
                p.add(counts);
            }
        }
        if replaying {
            continue;
        }
        let spec = ladder.best();
        // EACH TUBE'S OWN ROW IN ITS OWN FILE, from its own windows. A row
        // that averaged two counters would be a reading no instrument took.
        if let Some(lg) = loggers.get_mut(who) {
            let averages: Vec<Option<f64>> =
                spans.iter().map(|s| each[who].average(*s)).collect();
            match lg.add(clock::now(), counts, &averages) {
                Ok(Some(line)) => {
                    table.push_back(line.split('\t').map(str::to_string).collect());
                    while table.len() > TABLE_KEEP {
                        table.pop_front();
                    }
                    rows.push((who, line));
                }
                Ok(None) => {}
                Err(e) => table_note = format!("NOT LOGGING: {}", e),
            }
        }
        // Out to whoever is attached, after the borrow of the logger ends.
        for (k, line) in rows.drain(..) {
            feed.publish_row(k, &line);
        }
        if let Some(wl) = logging.as_ref() {
            if let Some(e) = wl.export_every {
                if exported.elapsed().as_secs_f64() >= e {
                    exported = Instant::now();
                    table_note = export_pages(&wl.dir, cpm_per_usvh);
                }
            }
        }

        let (h, width) = screen.size();
        // Everything above the table lays out as though the screen ended
        // where the table starts, and the footer stays on the last two rows.
        let tl = if wants_table { table_lines(h) } else { 0 };
        let hv = h - tl;
        out.clear();
        out.push_str("\x1b[2J");
        // The port, the firmware beside it, the serial. The program's own
        // name is not here: it goes in the top right corner, out of the way
        // of the readout, because a header that grows pushes the big number
        // right and the number is what people are looking at.
        let head = match (id.primary(), tubes) {
            (Some(c), 1) => format!(
                "{} @ {} baud   {}   serial {}",
                c.path, c.baud, c.version, c.serial_no
            ),
            (Some(c), n) => format!(
                "{} and {} more   {}   {} tubes averaged",
                c.path, n - 1, c.version, n
            ),
            (None, _) => "no counter".to_string(),
        };
        out.push_str(&format!(
            "{}{}{}{}",
            at(0, 0), DIM, head, OFF
        ));

        let mut row = 2usize;
        for &span in spans {
            match mean_cpm(&w, span, tubes) {
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
        let headline = mean_cpm(&w, 30.0, tubes)
            .or_else(|| mean_cpm(&w, *spans.last().unwrap(), tubes));
        let digits = big_number(&match headline {
            Some(v) => format!("{:.0}", v),
            None => "--".to_string(),
        });
        // Clear of the header, which is on the row the digits start on. This
        // was a constant 54 and the header outgrew it: a serial is fourteen
        // characters and the firmware sits beside the port, so the digits
        // were landing on top of the counter's own name. Measured, not
        // guessed.
        let left = digits_left(&head);
        // RESERVED, NOT MEASURED. The block is as wide as 65535 draws even
        // while the reading is 20, so the readout neither shifts under the
        // eye as a digit arrives nor -- which is worse -- gets dropped for
        // want of room at the one moment it is worth reading. Everything
        // optional on this screen is placed around this block, not before it.
        let reserve = readout_width();
        let readout = width >= left + reserve && h > BIG_ROWS;
        if readout {
            let tint = headline.map(|v| colour_for(level(v))).unwrap_or(DIM);
            for (i, d) in digits.iter().enumerate() {
                out.push_str(&format!("{}{}{}{}", at(i, left), tint, d, OFF));
            }
        }
        // The top right corner, which the readout reaches only on a narrow
        // terminal: what program this is and which version drew the screen.
        // It is the last thing placed and the first thing dropped.
        let plate = nameplate();
        let busy = if readout { left + reserve } else { head.chars().count() };
        if let Some(x) = nameplate_left(width, busy, plate.chars().count()) {
            out.push_str(&format!("{}{}{}{}", at(0, x), DIM, plate, OFF));
        }

        // Two rows of air, then the counts. It was three; the third is the
        // clock's, at the bottom, and the chart still clears the digits.
        row += 2;
        let counts_rows = if hv > row + 12 { 5 } else if hv > row + 8 { 3 } else { 1 };
        // THE COUNTS, COMPRESSING AS THEY AGE. A second a bar on the right,
        // then k seconds, then k*k and k*k*k, reaching back as far as the
        // spectrum's window -- see analysis::tiers. Four tiers of equal
        // width, so each one leftwards is another doubling and the strip
        // holds ten minutes where three tiers held five. One scale for all
        // four, so the same height is the same rate wherever it is drawn.
        let series: Vec<f64> = merged.iter().copied().collect();
        let strip = analysis::tiers_with(
            &series, dropped, width - 1,
            spec.window * tubes,
            analysis::TIERS + if tubes > 1 { 1 } else { 0 },
            1.0 / tubes as f64,
        );
        let peak = strip
            .iter()
            .flat_map(|t| t.values.iter().flatten())
            .cloned()
            .fold(0.0f64, f64::max);
        let every = logging.as_ref().map(|wl| wl.every.round().max(1.0) as i64);
        let n = (dropped + series.len()) as i64;
        let mut x0 = 0;
        for (ti, tier) in strip.iter().enumerate() {
            for (i, line) in bar_rows_to(&tier.values, counts_rows, peak).iter().enumerate() {
                out.push_str(&at(row + i, x0));
                let mut pen = "";
                for (x, &g) in line.iter().enumerate() {
                    if g == ' ' {
                        out.push(' ');
                        continue;
                    }
                    let rate = tier.values[x].unwrap_or(0.0) * 60.0;
                    let want = colour_for(level(rate));
                    if want != pen {
                        out.push_str(want);
                        pen = want;
                    }
                    out.push(g);
                }
                out.push_str(OFF);
            }
            // Above each tier, where it starts: F for each hand-over from a
            // finer tier, how long a bar is, and how far back the tier reaches.
            if row >= 1 && tier.columns > 2 {
                let label = format!(
                    "{}{}s/bar \u{b7} {}",
                    if ti == 0 { "" } else { "F " },
                    analysis::bar_seconds(tier.seconds),
                    analysis::span_words(tier.columns as f64 * tier.seconds)
                );
                let label: String = label.chars().take(tier.columns - 1).collect();
                let used = label.chars().count();
                out.push_str(&format!("{}{}{}{}", at(row - 1, x0), DIM, label, OFF));
                // Over the fine tier, a tick where each log row closes: the
                // frames the log is cut into, scrolling left with the counts.
                if let (Some(e), true) = (every, tier.seconds == 1.0) {
                    for j in used + 1..tier.columns {
                        let a = n - tier.columns as i64 + j as i64;
                        if a > 0 && a % e == 0 {
                            out.push_str(&format!("{}{}\u{a6}{}", at(row - 1, x0 + j), DIM, OFF));
                        }
                    }
                }
            }
            x0 += tier.columns;
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
        } else if hv > row + 4 {
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
            let bars = if hv > row + 9 { 5 } else if hv > row + 7 { 3 } else { 1 };
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
        if hv > row + 2 && width > 84 {
            row += 1;
            // ONLY THE PORT-HOLDER DRAWS. A client's pool is here for the
            // countdown and nothing else; its line arrives as an event, from
            // the one process that also wrote it to the emission log.
            let ready = if feed.owns_the_port() {
                pools.iter().position(|p| p.ready())
            } else {
                None
            };
            if let Some(k) = ready {
                let (top, _) = spec.loudest();
                suspect = top > 0.0 && top >= spec.chance_max() * 1.25;
                let (text, record) = pools[k].draw();
                let at_time = clock::format(clock::now(), "%H:%M:%S");
                let serial = id.counters.get(k).map(|c| c.serial_no.as_str()).unwrap_or("");
                if let Some(wl) = logging.as_ref() {
                    let _ = entropy::write_record(&wl.dir, &record, serial, suspect);
                }
                feed.publish_random(k, &text, &at_time, suspect);
                shown = Some((text.clone(), at_time));
            }
            match &shown {
                Some((text, at_time)) => {
                    out.push_str(&format!("{}{}random   {}{}",
                                          at(row, 0), CYAN,
                                          entropy::group_hex(text), OFF));
                    if hv > row + 2 {
                        // THE COUNTDOWN KEEPS RUNNING. The line stayed on
                        // screen with no indication of whether the next one
                        // was a minute away or eight, which is the one thing
                        // somebody watching it wants to know.
                        let note = format!(
                            "{} bits from decay at {}   {}{}",
                            entropy::ENTROPY_BITS as i64, at_time,
                            entropy::pool_status(&pools[0], "next in "),
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
                    entropy::pool_status(&pools[0], "next in "), OFF
                )),
            }
            // The time now, two rows under the random line whether or not
            // it has a note yet, so it does not jump when the first line
            // arrives -- and so a screenshot says when it was taken.
            if row + 2 < hv.saturating_sub(2) {
                out.push_str(&format!("{}{}clock{}    {}", at(row + 2, 0), DIM, OFF,
                                      clock::format(clock::now(), "%Y-%m-%d %H:%M:%S")));
                // Who else is reading this counter through us. Worth a word
                // because the whole point of the socket is invisible
                // otherwise -- a GUI attaching shows up here and nowhere else.
                match feed.watchers() {
                    0 => {}
                    1 => out.push_str(&format!("{}{}1 attached{}", at(row + 2, 24), DIM, OFF)),
                    n => out.push_str(&format!("{}{}{} attached{}", at(row + 2, 24), DIM, n, OFF)),
                }
                // What the log is doing, beside it: what the backfill found,
                // or that rows are not reaching the disk.
                if !table_note.is_empty() && width > 40 + table_note.len() {
                    let tint = if table_note.starts_with("NOT") { YELLOW } else { DIM };
                    out.push_str(&format!("{}{}{}{}", at(row + 2, 32), tint, table_note, OFF));
                }
            }
        }

        if tl > 0 {
            draw_table(&mut out, h - 2 - tl, tl, width, &names, &table);
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
            // HOW LONG THIS HAS BEEN WATCHING, not how much counter it holds.
            // The two were the same number until a monitor could be handed
            // eight hours of history on the way in, at which point
            // `--duration 10` stopped after the replay and before the first
            // live sample.
            if watching.elapsed().as_secs_f64() >= d {
                break;
            }
        }
    }
    for (k, lg) in loggers.iter_mut().enumerate() {
        let averages: Vec<Option<f64>> = spans.iter().map(|s| each[k].average(*s)).collect();
        lg.finish(&averages);
    }
    if let Some(wl) = logging.as_ref() {
        if wl.export_every.is_some() {
            export_pages(&wl.dir, cpm_per_usvh);
        }
    }
    // Only if this end started it: a monitor closing must not silence the
    // counter for a service that is still logging it.
    feed.stop();
}

/// Rows of log the monitor keeps for its table: more than any screen shows.
const TABLE_KEEP: usize = 200;

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
           devices: &[String], baud: Option<u32>,
           logs: Option<std::path::PathBuf>,
           backfill: Option<(usize, f64)>) -> i32 {
    install_stop_handler();

    let started = clock::now();
    let mut waiting = false;
    let found = loop {
        let (found, why) = counter::find_all(devices, baud);
        match why {
            None => break found,
            Some(e) => {
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

    // EVERY COUNTER BACKFILLED FROM ITS OWN FLASH, into its own file. Two
    // tubes were both in the room while nobody was listening and both wrote
    // down what they saw; the records stay apart, because the only way to ask
    // whether two instruments agree is to have kept both their answers.
    if let Some((bytes, max_gap)) = backfill {
        log::write_status("backfilling from the counters' history");
        for c in &found {
            println!("radbeeper: {}",
                     backfill_at_start(c, &dir, spans, every, bytes, max_gap, false));
        }
    }

    let tubes = found.len();
    log::write_status(&format!(
        "monitoring {} ({})",
        found.iter().map(|c| c.path.as_str()).collect::<Vec<_>>().join(", "),
        found.iter().map(|c| c.version.as_str()).collect::<Vec<_>>().join(", ")
    ));
    let mut bank = Bank::open(found);
    let id = bank.identity(spans);
    // ONE SET OF WINDOWS AND ONE LOG PER TUBE. The service records; it does
    // not average. Averaging is a question about a display, and every display
    // that asks it can do so from the stream -- but a row that blended two
    // instruments would be a reading neither of them took, and no later
    // analysis could unpick it.
    let mut each: Vec<Windows> = (0..tubes).map(|_| Windows::new(spans)).collect();
    let mut loggers: Vec<Logger> = id
        .counters
        .iter()
        .map(|c| Logger::new(spans, dir.clone(), &c.serial_no, every))
        .collect();
    // THE FAN-OUT. This process holds the flocks, so it owes the stream to
    // everybody who wants a counter and cannot have the port: a monitor, a
    // second monitor, a GUI. A socket that will not bind is not fatal -- the
    // logging is the job, and it carries on -- but it is worth saying,
    // because the symptom otherwise is a window printing `port busy` with no
    // explanation of why the stream it expected was not there.
    let mut srv = match broker::Server::start(&dir, &id) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("radbeeper: not serving the stream -- {}", e);
            eprintln!("    the log is unaffected; a monitor will need the port itself");
            None
        }
    };
    // THE POOLS BELONG TO WHOEVER HOLDS THE PORTS, and until now nothing held
    // them for long: the pool lived in the monitor, so closing the window
    // threw away however many minutes of measured entropy it had gathered.
    // Here one fills per tube for as long as that tube is plugged in, and the
    // line it draws is written once, by this process, and published to every
    // window watching. Per tube and never merged: an emission is an audit
    // record of ONE source, and `radbeeper random --check` recomputes it from
    // that source's own counts.
    let mut pools: Vec<entropy::Entropy> =
        (0..tubes).map(|_| entropy::Entropy::default()).collect();
    // The ladder is fed the whole-second SUM across the tubes: a period is a
    // property of the room, both counters are looking at the same room, and
    // adding them is twice the signal on one time base. `suspect` is an
    // annotation on the emission record, and one written without it would be
    // quietly claiming a flat spectrum nobody checked.
    let mut ladder = Ladder::new();
    let mut sec_bin: Option<i64> = None;
    let mut sec_sum: u32 = 0;

    if let Some(s) = srv.as_mut() {
        let names = log::columns(&log::header(spans));
        let mut seed: Vec<(usize, String)> = Vec::new();
        for (k, c) in id.counters.iter().enumerate() {
            let path = log::path(clock::now(), &dir, Some(&c.serial_no));
            for (_, cells) in
                log::read_table(&path, &names).into_iter().rev().take(TABLE_KEEP).rev()
            {
                seed.push((k, cells.join("\t")));
            }
        }
        s.seed_rows(seed.into_iter());
    }

    let here = log::site_at(
        id.counters.first().map(|c| c.serial_no.as_str()).unwrap_or(""),
        clock::now(),
        &loggers.first().map(|l| l.sites.clone()).unwrap_or_default(),
    );
    for c in &id.counters {
        println!("radbeeper: monitoring {} -- {}", c.path, c.version);
        println!("radbeeper: counter {} logging to {}", c.serial_no,
                 log::path(clock::now(), &dir, Some(&c.serial_no)).display());
    }
    println!("radbeeper: {} at {}", if tubes == 1 { "counter" } else { "counters" },
             here.unwrap_or_else(|| "an unrecorded place".to_string()));
    println!("radbeeper: a row every {}s", log::g(every));
    if let Some(s) = srv.as_ref() {
        println!("radbeeper: serving the stream on {}", s.socket().display());
        println!("radbeeper: a monitor attaches to it -- the ports stay here");
    }

    // Every port conversation is over -- the backfill above ran before the
    // bank existed -- so the counters can start streaming.
    bank.start();
    loop {
        // Whoever turned up while we were waiting for this second. Before the
        // read, so a window that has just opened is greeted within a second
        // rather than after the next sample.
        if let Some(s) = srv.as_mut() {
            s.accept_pending();
        }
        // EVERY SENDER GONE MEANS EVERY COUNTER GONE. One tube unplugged ends
        // its thread and nothing else; the bank is finished only when the
        // last one has stopped talking.
        let (who, when, counts) = match bank.next(Duration::from_millis(2500)) {
            Some(v) => v,
            None => break,
        };
        each[who].add(when, counts);
        if let Some(s) = srv.as_mut() {
            s.publish_sample(who, when, counts);
        }
        // The spectrum's second closes when the wall clock's does, and takes
        // whatever every tube put in it.
        let this = when.floor() as i64;
        match sec_bin {
            Some(b) if b == this => sec_sum += counts,
            Some(_) => {
                ladder.add(sec_sum);
                sec_bin = Some(this);
                sec_sum = counts;
            }
            None => {
                sec_bin = Some(this);
                sec_sum = counts;
            }
        }
        pools[who].add(counts);
        // The line, when a pool has earned it: written once, here, against the
        // tube that earned it, and sent to every window so they all show the
        // same hex.
        if pools[who].ready() {
            let spec = ladder.best();
            let (top, _) = spec.loudest();
            let suspect = top > 0.0 && top >= spec.chance_max() * 1.25;
            let (text, record) = pools[who].draw();
            let at_time = clock::format(clock::now(), "%H:%M:%S");
            let _ = entropy::write_record(&dir, &record, &id.counters[who].serial_no, suspect);
            if let Some(s) = srv.as_mut() {
                s.publish_random(who, &text, &at_time, suspect);
            }
        }
        let averages: Vec<Option<f64>> =
            spans.iter().map(|s| each[who].average(*s)).collect();
        match loggers[who].add(when, counts, &averages) {
            Ok(Some(line)) => {
                if let Some(s) = srv.as_mut() {
                    s.publish_row(who, &line);
                }
            }
            Ok(None) => {}
            Err(e) => {
                eprintln!("radbeeper: could not write the log -- {}", e);
                break;
            }
        }
        if stopping() {
            break;
        }
        // --duration is what makes this path testable at all: without it the
        // only way to exercise the logger is to start a daemon and kill it,
        // which is not something a test suite should do.
        if duration.map(|d| each[who].elapsed() >= d).unwrap_or(false) {
            break;
        }
    }
    for (k, lg) in loggers.iter_mut().enumerate() {
        let averages: Vec<Option<f64>> = spans.iter().map(|s| each[k].average(*s)).collect();
        lg.finish(&averages);
    }
    bank.stop();
    log::write_status("stopped");
    0
}

/// Read the tail of the counter's flash into the log, before anything live is
/// appended: backfilled rows belong in the past and the merge rewrites the
/// file. The service starts when a counter is plugged in or the machine comes
/// up, and the monitor when somebody sits down at it -- which is exactly when
/// the counter has been recording somewhere this log was not. The flash is a
/// ring, and the gaps are wherever nothing was listening: `history::backfill`
/// finds the newest end of the ring and fills only the slots that are empty.
///
/// One line saying what happened, for whoever is showing it.
fn backfill_at_start(c: &counter::Counter, dir: &Path, spans: &[f64], every: f64,
                     bytes: usize, max_gap: f64, quiet: bool) -> String {
    let offset = measure_clock_offset(c).unwrap_or(0.0);
    let blob = read_history_tail(c, bytes, quiet);
    if blob.is_empty() {
        return "backfill skipped -- the counter returned no history".to_string();
    }
    let sites = log::read_sites(dir);
    let r = history::backfill(&blob, spans, every, max_gap, offset, dir,
                              Some(c.serial_no.as_str()), &sites, None);
    format!("backfill -- {} samples, {} rows, {} added, {} already logged",
            r.samples, r.rows, r.added, r.clashed)
}

/// Rows to the dated log, one every `every` seconds.
///
/// What `service` writes, shared, so the monitor writing the log while it is
/// open cannot become a second dialect of the same file.
struct Logger {
    dir: PathBuf,
    serial: String,
    every: f64,
    out: log::Writer,
    iv: log::Interval,
    due: Option<f64>,
    sites: Vec<(String, f64, String)>,
    sites_mtime: Option<u64>,
}

impl Logger {
    fn new(spans: &[f64], dir: PathBuf, serial: &str, every: f64) -> Logger {
        // The site is re-read whenever sites.tsv changes, so `radbeeper site`
        // takes effect on the next row rather than at the next restart.
        let sites = log::read_sites(&dir);
        let sites_mtime = mtime_of(&dir.join("sites.tsv"));
        Logger {
            out: log::Writer::new(spans, dir.clone(), Some(serial.to_string()), every),
            iv: log::Interval::new(spans.len()),
            dir,
            serial: serial.to_string(),
            every,
            due: None,
            sites,
            sites_mtime,
        }
    }

    /// One second. Returns the row when this second completed an interval
    /// and the row was written -- not when its slot was already taken.
    fn add(&mut self, when: f64, counts: u32, averages: &[Option<f64>])
        -> std::io::Result<Option<String>>
    {
        self.iv.add(counts, averages, 1.0);
        let due = *self.due.get_or_insert(when + self.every);
        // One write and one flush per interval instead of per second: at the
        // default that is two syscalls a minute rather than a hundred and
        // twenty, which is the whole difference on a Pi Zero logging to an SD
        // card. Nothing is buffered up to pay for it.
        if when < due {
            return Ok(None);
        }
        let line = self.row(averages);
        let wrote = self.out.write(clock::now(), &line)?;
        self.iv.reset();
        self.due = Some(when + self.every);
        Ok(wrote.then_some(line))
    }

    /// Whatever the last interval collected is worth keeping: stopped four
    /// seconds after a spike, the spike should still be on disk, and the
    /// seconds column says the row is short.
    fn finish(&mut self, averages: &[Option<f64>]) {
        if self.iv.seconds > 0.0 {
            let line = self.row(averages);
            let _ = self.out.write(clock::now(), &line);
        }
        self.out.close();
    }

    fn row(&mut self, averages: &[Option<f64>]) -> String {
        let now = clock::now();
        let m = mtime_of(&self.dir.join("sites.tsv"));
        if m != self.sites_mtime {
            self.sites_mtime = m;
            self.sites = log::read_sites(&self.dir);
        }
        let site = log::site_at(&self.serial, now, &self.sites).unwrap_or_default();
        log::row(now, self.iv.cps(), self.iv.counts, self.iv.seconds,
                 averages, &self.iv.peaks, log::SRC_LIVE, &site)
    }
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

/// What a failed find means to `--wait`.
enum Verdict {
    /// Look again. The port is busy, and busy is the whole point of waiting.
    Retry,
    /// Stop, and let the find's own message stand -- this is not a wait
    /// problem and a wait will not fix it.
    Give,
    /// Stop, with a line to add: the wait itself ran out.
    Expired(String),
}

/// Whether a busy port is worth asking about again.
///
/// WHY ONLY BUSY IS WAITED ON. "No counter" is not a condition that clears by
/// standing still: an unplugged counter, a kernel with no ch341, a typo in
/// --device -- none of them are fixed by asking a second time, and a `watch`
/// that sat there on a misspelt device would be indistinguishable from one
/// that had hung. A locked port is the one failure that somebody in another
/// window is about to fix, so it is the one that is waited on.
///
/// Split from the loop because a decision is testable and a loop with a sleep
/// in it is not: this is what says the timeout message gets written.
fn wait_verdict(e: &counter::NotFound, waited: f64, limit: f64) -> Verdict {
    if !e.busy {
        return Verdict::Give;
    }
    if waited >= limit {
        return Verdict::Expired(format!(
            "Waited {}s for the port and it never came free.",
            log::g(limit)));
    }
    Verdict::Retry
}

/// Put SIGINT and SIGTERM back the way they were found.
///
/// `--wait` installs the stop handler so that Ctrl-C answers the wait. The
/// commands the wait runs *before* are not all written to read that flag --
/// `cpm` sits on the counter for thirty seconds and never looks at it -- and a
/// Ctrl-C swallowed for thirty seconds is worse than no handler at all. So the
/// handler lives exactly as long as the waiting does, and whatever runs next
/// installs its own if it wants one. `watch` does.
fn restore_stop_default() {
    unsafe {
        libc::signal(libc::SIGTERM, libc::SIG_DFL);
        libc::signal(libc::SIGINT, libc::SIG_DFL);
    }
}

/// `counter::find`, but a busy port is waited on instead of refused.
///
/// WHY THIS IS NOT THE DEFAULT. The error it replaces names the command that
/// hands the counter over, and a person who has not read that yet is better
/// served by reading it than by watching a program sit still. Waiting is what
/// you want in the one case where you already know -- the service is logging,
/// you are about to stop it, and you would rather not race it to the port.
///
/// The race is real, and it is not with the service: `service` polls the same
/// port every SERVICE_WAIT seconds and takes it back the moment it is free. A
/// stopped service is not in the running, but a RESTARTED one is, and then the
/// two are simply both asking. Whoever asks first wins.
fn find_waiting(device: Option<&str>, baud: Option<u32>, limit: f64)
                -> Result<counter::Counter, counter::NotFound> {
    // So Ctrl-C during the wait is an answer and not a kill: the flag is read
    // between polls, which is also where the process is doing nothing.
    install_stop_handler();
    let started = clock::now();
    let mut said = false;
    loop {
        let e = match counter::find(device, baud) {
            Ok(c) => {
                restore_stop_default();
                // A Ctrl-C that landed between the last check and the port
                // coming free is still a Ctrl-C, and it still means stop.
                if stopping() {
                    println!("radbeeper: stopped waiting.");
                    std::process::exit(1);
                }
                if said {
                    println!("radbeeper: the port came free.");
                }
                return Ok(c);
            }
            Err(e) => e,
        };
        match wait_verdict(&e, clock::now() - started, limit) {
            Verdict::Give => {
                restore_stop_default();
                return Err(e);
            }
            Verdict::Expired(note) => {
                // Appended rather than substituted: the detail is the part
                // that names `doas rc-service radbeeper stop`, and a wait that
                // timed out is exactly when that is worth reading.
                restore_stop_default();
                let mut e = e;
                e.detail = format!("{}\n{}", e.detail, note);
                return Err(e);
            }
            Verdict::Retry => {}
        }
        if !said {
            said = true;
            let how_long = if limit.is_finite() {
                format!(" up to {}s", log::g(limit))
            } else {
                String::new()
            };
            println!("radbeeper: the port is busy -- waiting{} for it. \
                      Ctrl-C to stop.", how_long);
        }
        if stopping() {
            println!("radbeeper: stopped waiting -- the port is still busy.");
            std::process::exit(1);
        }
        std::thread::sleep(Duration::from_secs_f64(WAIT_POLL));
    }
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
    println!("  radbeeper watch            the monitor, logging while it is open");
    println!("  radbeeper service          log every counter found, and serve the stream");
    println!("  radbeeper random           256 bits of hex, out of decay timing");
    println!("  radbeeper random --check F recompute every line in an emission log");
    println!("  radbeeper backfill         fill the log's gaps from the counter's flash");
    println!("  radbeeper log info|pull    what history it holds, or download it");
    println!("  radbeeper export           index.html and random.html, from the logs");
    println!("  radbeeper hotplug          sit in the session, open the monitor on plug-in");
    println!();
    println!("  -d, --device PATH          serial port; repeat it for a second counter");
    println!("  -b, --baud RATE            baud (default: try 115200 then 57600)");
    println!("      --spans 3,30,300,3000,30000  averaging windows, seconds");
    println!("      --cpm-per-usvh N       tube factor (default {})", counter::DEFAULT_CPM_PER_USVH);
    println!("      --duration SECONDS     stop after this long");
    println!("      --wait [SECONDS]       a busy port: wait for it, not give up");
    println!("      --log-every SECONDS    row spacing for service (default {})",
             log::g(log::DEFAULT_LOG_EVERY));
    println!("      --logs DIR             where service, watch and backfill write, and export reads");
    println!("      --no-log               watch without writing anything");
    println!("      --no-export            watch without writing index.html (hourly, and on quit)");
    println!("      --no-backfill          skip reading the counter's history at start");
    println!("      --image FILE           backfill from a saved .bin, no counter");
    println!("      --serial SERIAL        which counter an image came from");
    println!("      --bytes N              how much flash to read");
    println!("      --max-gap SECONDS      a longer hole ends the averages");
    println!("      --poll SECONDS         hotplug: how often /dev is read (default 4)");
    println!("      --settle SECONDS       hotplug: grace before a new node is opened (default 2)");
    println!("      --tries N              hotplug: attempts per plug event (default 3)");
    println!("      --tui                  hotplug: a terminal, even where radbeeper-gui is installed");
    println!("  -o, --output STEM          where log pull writes .bin and .csv");
    println!("  -o, --output FILE          export: where the page goes (default index.html)");
    println!("      --title TEXT           export: the page's heading");
    println!("      --random-output FILE   export: the audit page (default random.html beside it)");
    println!("      --no-random-page       export: do not write the audit page");
    println!();
    println!("site and recompute are in the");
    println!("Python program in the same repository. They are being ported; the");
    println!("log format is here already, and tests/test_differential.py is what");
    println!("says it is the same format and not a second dialect of it.");
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut command = String::new();
    // REPEATABLE, BECAUSE TWO TUBES ARE A SUPPORTED ARRANGEMENT. `-d A -d B`
    // names both; no `-d` at all searches /dev and takes everything that
    // answers. One `-d` behaves exactly as it always did.
    let mut devices: Vec<String> = Vec::new();
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
    let mut no_log = false;
    let mut no_export = false;
    let mut wait: Option<f64> = None;
    let mut set_clock = false;
    let mut output: Option<String> = None;
    // hotplug's three. Four seconds is a read of /dev fifteen times a minute,
    // which costs nothing; settle is the grace a node gets between appearing
    // and being opened, and tries is how many times one plug event is worth
    // retrying before it is written off.
    let mut poll: f64 = 4.0;
    let mut settle: f64 = 2.0;
    let mut tries: u32 = 3;
    let mut tui = false;
    let mut log_action = "info".to_string();
    let mut serial: Option<String> = None;
    let mut title = radbeeper::export::DEFAULT_TITLE.to_string();
    let mut random_output: Option<PathBuf> = None;
    let mut no_random_page = false;

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
            "-d" | "--device" => devices.extend(next(&mut i)),
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
            "--wait" => {
                // AN OPTIONAL VALUE, which is a thing this parser does
                // nowhere else: `--wait` on its own waits for as long as it
                // takes, `--wait 30` gives up after thirty seconds. Peeking at
                // the next argument is unambiguous only because no command
                // this program has is a number, and none can become one --
                // `--wait watch` and `--wait 30 watch` both read correctly.
                wait = Some(match args.get(i + 1).and_then(|v| v.parse::<f64>().ok()) {
                    Some(secs) => {
                        i += 1;
                        secs
                    }
                    None => f64::INFINITY,
                });
            }
            "--logs" => logs = next(&mut i).map(std::path::PathBuf::from),
            "--log-every" => {
                log_every = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(log_every)
            }
            "--check" => check = next(&mut i).map(std::path::PathBuf::from),
            "--image" => image = next(&mut i).map(std::path::PathBuf::from),
            "--serial" => serial = next(&mut i),
            "--bytes" | "--backfill-bytes" => bytes = next(&mut i).and_then(|v| v.parse().ok()),
            "--no-backfill" => no_backfill = true,
            "--no-log" => no_log = true,
            "--no-export" => no_export = true,
            "--set" => set_clock = true,
            "--max-gap" => {
                max_gap = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(max_gap)
            }
            "-o" | "--output" => output = next(&mut i),
            "--poll" => poll = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(poll),
            "--settle" => settle = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(settle),
            "--tries" => tries = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(tries),
            "--tui" => tui = true,
            "--title" => title = next(&mut i).unwrap_or(title),
            "--random-output" => random_output = next(&mut i).map(PathBuf::from),
            "--no-random-page" => no_random_page = true,
            "info" | "pull" if command == "log" => log_action = a.to_string(),
            "site" => {
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
            devices.first().map(|s| s.as_str()), baud, logs, image.as_deref(),
            serial.as_deref(), output.as_deref(),
        ));
    }
    if command == "export" {
        std::process::exit(radbeeper::export::run(
            &logs.unwrap_or_else(log::state_dir),
            Path::new(output.as_deref().unwrap_or("index.html")),
            !no_random_page, cpm_per_usvh, &title, random_output.as_deref(),
        ));
    }
    if command == "log" {
        std::process::exit(log_cmd(&log_action, bytes, output.as_deref(),
                                   devices.first().map(|s| s.as_str()), baud));
    }
    if command == "random" {
        // --check needs no hardware, so it runs before anything looks for a
        // counter: an audit of a file from another machine is the ordinary
        // case, not an odd one.
        if let Some(p) = check {
            std::process::exit(check_random(&p));
        }
        std::process::exit(random(&spans, duration, devices.first().map(|s| s.as_str()), baud, logs));
    }
    if command == "hotplug" {
        let dir = logs.clone().unwrap_or_else(log::state_dir);
        std::process::exit(hotplug(&dir, devices.first().map(|s| s.as_str()), baud, poll, settle, tries,
                                   duration, tui));
    }
    if command == "service" {
        let backfill = (!no_backfill).then(|| (bytes.unwrap_or(64 * 1024), max_gap));
        std::process::exit(service(
            &spans, log_every, duration, &devices, baud, logs, backfill,
        ));
    }

    // WATCH ASKS THE SOCKET BEFORE IT ASKS /dev, so it is settled here rather
    // than after a port has been opened. If a service is logging this counter
    // the monitor attaches to it and the port is never touched; if nobody is,
    // the monitor takes the port and serves it in turn, and the next window
    // attaches to this one.
    if command == "watch" {
        let dir = logs.clone().unwrap_or_else(log::state_dir);
        let logging = (!no_log).then(|| WatchLog {
            dir: dir.clone(),
            every: log_every,
            backfill: (!no_backfill).then(|| (bytes.unwrap_or(64 * 1024), max_gap)),
            export_every: (!no_export).then_some(3600.0),
        });
        let mut feed = match Feed::open(&dir, &devices, baud, &spans, wait) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("{}: {}", if e.busy { "port busy" } else { "no counter" }, e.reason);
                for line in e.detail.lines() {
                    eprintln!("    {}", line);
                }
                std::process::exit(1);
            }
        };
        watch(&mut feed, &spans, cpm_per_usvh, duration, logging);
        return;
    }

    let found = match wait {
        Some(limit) => find_waiting(devices.first().map(|s| s.as_str()), baud, limit),
        None => counter::find(devices.first().map(|s| s.as_str()), baud),
    };
    let c = match found {
        Ok(c) => c,
        Err(e) => {
            // A BUSY PORT IS NOT A DEAD END FOR `probe` ANY MORE. The service
            // holding the counter introduced itself the moment we connected,
            // and its greeting is most of what probe prints. What is missing
            // is the battery and the clock, which are questions for the
            // counter -- and interrupting a stream mid-second to ask one
            // costs the log a sample, which a status command has no business
            // doing to a logger.
            if e.busy && command == "probe" {
                let dir = logs.clone().unwrap_or_else(log::state_dir);
                if let Some(client) = broker::Client::attach(&dir)
                    .filter(|c| devices.iter().all(|d| c.identity().serves(d)))
                {
                    let id = client.identity();
                    for (k, c) in id.counters.iter().enumerate() {
                        if k > 0 {
                            println!();
                        }
                        println!("counter    {}", c.version);
                        println!("model      {}", counter::model_of(&c.version));
                        println!("serial     {}", c.serial_no);
                        println!("port       {} @ {} baud", c.path, c.baud);
                    }
                    println!("held by    the radbeeper serving {}",
                             broker::socket_path(&dir).display());
                    println!("           battery and clock need the port itself:");
                    println!("           doas rc-service radbeeper stop");
                    println!("reading    radbeeper watch  attaches to it -- no handover needed");
                    return;
                }
            }
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
/// Is there a counter here worth opening a window for?
///
/// A COUNTER THAT IS BEING SERVED COUNTS, and this is the line that used to
/// make the two halves of the design deadlock. udev starts the service, the
/// service takes the port, and this then asked the PORT whether there was a
/// counter -- got `busy`, and gave up. On a machine where the logger works,
/// which is every machine it was designed for, the window could therefore
/// never open at all.
///
/// The existence of the socket is enough and a connection is not attempted:
/// greeting a client costs the server its whole history, which is not a price
/// to pay fifteen times a minute for a yes/no. A STALE node gives a false yes,
/// and that is harmless -- the monitor it opens tries the socket, finds
/// nothing behind it and takes the port itself, which is what should happen.
fn worth_a_window(dir: &Path, device: Option<&str>, baud: Option<u32>) -> bool {
    if broker::socket_path(dir).exists() {
        return true;
    }
    match counter::find(device, baud) {
        Ok(_) => true,
        Err(e) => {
            if !e.busy {
                log::write_status(&format!("dormant: {}", e.reason));
            }
            false
        }
    }
}

fn open_window(dir: &Path, device: Option<&str>, baud: Option<u32>, tui: bool) -> Option<Child> {
    if !worth_a_window(dir, device, baud) {
        return None;
    }
    // THE WINDOW A PERSON WOULD HAVE OPENED THEMSELVES. If radbeeper-gui is
    // installed, that is a window on its own and needs no terminal wrapped
    // round it; if it is not, a terminal running the monitor is the window,
    // and that is what this did before the GUI existed. Both attach to
    // whatever is serving the stream, so the choice is cosmetic and `--tui`
    // takes it back.
    if !tui {
        if let Some(g) = find_gui() {
            return spawn_window(Command::new(g));
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
    spawn_window(cmd)
}

/// Start a window and let go of it.
fn spawn_window(mut cmd: Command) -> Option<Child> {
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
            eprintln!("radbeeper: could not open a window: {}", e);
            None
        }
    }
}

/// radbeeper-gui, if it is installed.
///
/// BESIDE US FIRST, then PATH. The two are built from one checkout and land in
/// the same directory; a session that has an old radbeeper-gui somewhere on
/// PATH and a new pair in ~/.local/bin should get the pair.
fn find_gui() -> Option<PathBuf> {
    if let Ok(me) = std::env::current_exe() {
        if let Some(dir) = me.parent() {
            let beside = dir.join("radbeeper-gui");
            if beside.is_file() && is_executable(&beside) {
                return Some(beside);
            }
        }
    }
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join("radbeeper-gui"))
        .find(|p| p.is_file() && is_executable(p))
}

/// `radbeeper hotplug`: open the monitor when a counter appears -- now, or in
/// an hour's time.
fn hotplug(
    dir: &Path,
    device: Option<&str>,
    baud: Option<u32>,
    poll: f64,
    settle: f64,
    tries: u32,
    duration: Option<f64>,
    tui: bool,
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
            child = open_window(dir, device, baud, tui);
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
    /// THE BUG THIS RELEASE EXISTS FOR, pinned: a counter held by a service
    /// that is serving the stream is worth a window. The port is busy, there
    /// is no device to find -- and it must still say yes, because the monitor
    /// it opens will attach rather than ask for the port.
    #[test]
    fn a_counter_being_served_is_worth_a_window_though_the_port_is_busy() {
        let dir = std::env::temp_dir().join(format!("radbeeper-window-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = broker::socket_path(&dir);
        let _ = std::fs::remove_file(&sock);
        // No socket and a device that cannot exist: nothing to open.
        assert!(!worth_a_window(&dir, Some("/dev/null-no-counter-here"), None));
        // A server on the socket, and the same impossible device: still yes.
        std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert!(worth_a_window(&dir, Some("/dev/null-no-counter-here"), None));
        let _ = std::fs::remove_file(&sock);
    }

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
    fn the_readout_has_room_for_everything_the_counter_can_say() {
        // Two bytes of CPM is 0..=65535, and every one of them has to fit in
        // the block the readout reserves -- at the reference width of 160
        // columns, beside the longest header a 320 produces.
        let reserve = readout_width();
        for v in [0u16, 7, 42, 999, 9999, u16::MAX] {
            let drawn = big_number(&v.to_string())
                .iter()
                .map(|r| r.chars().count())
                .max()
                .unwrap_or(0);
            assert!(drawn <= reserve, "{} needs {} of {}", v, drawn, reserve);
        }
        assert!(digits_left(HEAD) + reserve <= 160,
                "the readout does not fit the terminal the shots are taken in");
    }

    #[test]
    fn the_nameplate_names_the_program_and_its_version() {
        // Out of the manifest, not a string somebody remembers to bump.
        let plate = nameplate();
        assert!(plate.starts_with(env!("CARGO_PKG_NAME")));
        assert!(plate.ends_with(env!("CARGO_PKG_VERSION")));
    }

    #[test]
    fn the_nameplate_yields_the_corner_to_the_readout() {
        let plate = nameplate().chars().count();
        let left = digits_left(HEAD);
        let busy = left + readout_width();
        // 160 columns: the corner is free and the plate sits in it.
        let x = nameplate_left(160, busy, plate).expect("no corner at 160");
        assert_eq!(x + plate, 160, "not against the right edge");
        assert!(x > busy, "drawn over the readout");
        // Narrow enough that the readout wants those columns: nothing.
        assert_eq!(nameplate_left(busy + plate, busy, plate), None);
        assert_eq!(nameplate_left(40, busy, plate), None);
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

    /// `--wait`, which is one decision taken over and over: is this failure
    /// one that standing still will fix?
    fn failure(busy: bool) -> counter::NotFound {
        counter::NotFound {
            reason: "r".into(),
            detail: "d".into(),
            busy,
        }
    }

    #[test]
    fn a_busy_port_is_waited_on() {
        assert!(matches!(wait_verdict(&failure(true), 0.0, 30.0), Verdict::Retry));
        assert!(matches!(wait_verdict(&failure(true), 29.9, 30.0), Verdict::Retry));
    }

    /// The case that would otherwise turn a typo into a hang: nothing about an
    /// absent counter changes because a program asked twice.
    #[test]
    fn a_missing_counter_is_not_waited_on_at_all() {
        assert!(matches!(wait_verdict(&failure(false), 0.0, 30.0), Verdict::Give));
        assert!(matches!(wait_verdict(&failure(false), 0.0, f64::INFINITY), Verdict::Give));
    }

    #[test]
    fn the_limit_ends_the_wait_and_says_how_long_it_was() {
        match wait_verdict(&failure(true), 30.0, 30.0) {
            Verdict::Expired(note) => assert!(note.contains("30s"), "got {}", note),
            _ => panic!("30s of a 30s wait is the end of it"),
        }
    }

    /// `--wait` with no number is a wait with no end, and an hour in it is
    /// still waiting -- that is the whole difference from `--wait 3600`.
    #[test]
    fn an_unbounded_wait_does_not_expire() {
        assert!(matches!(wait_verdict(&failure(true), 3600.0, f64::INFINITY),
                         Verdict::Retry));
    }
}
