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
use radbeeper::export::bytes_text;
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
/// How often a running service looks for a counter that was not there before.
///
/// ONE LOG CYCLE. Often enough that plugging a tube in and looking at the
/// screen feels like one action, rare enough that the scan -- an open and a
/// close on every candidate port -- costs nothing anybody can measure.
const RESCAN: f64 = log::DEFAULT_LOG_EVERY;
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
    /// Whether the raw seconds behind each emission are kept beside it.
    ///
    /// ON BY DEFAULT AND TURNED OFF BY A FLAG, because the file is the
    /// evidence for a claim the program makes on its own front page -- that
    /// sixty-four characters came out of decay -- and a default that dropped
    /// the evidence would leave the claim uncheckable unless somebody had
    /// thought to ask for it in advance. It costs about a byte a second.
    frames: bool,
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
                                    radbeeper::export::DEFAULT_TITLE, None,
                                    radbeeper::audit::DEFAULT_FRAME_BUDGET) {
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
/// What a reader thread sends back.
enum Tube {
    Sample { who: usize, when: f64, counts: u32 },
    /// This tube stopped answering. Its slot stays reserved for its serial.
    Gone { who: usize },
}

struct Bank {
    /// One slot per counter ever seen, `None` once it has gone quiet -- which
    /// drops its `Counter` and with it the file descriptor and the flock, so
    /// the port can be taken again when the thing is plugged back in.
    counters: Vec<Option<std::sync::Arc<counter::Counter>>>,
    /// Who each slot IS, kept whether or not it is answering. A tube that
    /// comes back keeps the index its serial had, so the logs, the colours
    /// and the sample tags all stay pointing at the same instrument.
    ids: Vec<broker::CounterId>,
    /// Held for the lifetime of the bank so the channel never reports itself
    /// disconnected: an empty bank is one waiting for a counter, not one that
    /// is finished.
    tx: std::sync::mpsc::Sender<Tube>,
    rx: std::sync::mpsc::Receiver<Tube>,
    running: bool,
}

impl Bank {
    fn open(found: Vec<counter::Counter>) -> Bank {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut bank = Bank { counters: Vec::new(), ids: Vec::new(), tx, rx, running: false };
        for c in found {
            bank.adopt(c);
        }
        bank
    }

    /// Take a counter into the bank, reusing its slot if this serial has been
    /// here before, and return the index it landed in.
    fn adopt(&mut self, c: counter::Counter) -> usize {
        let id = broker::CounterId {
            path: c.path.clone(),
            baud: c.baud,
            version: c.version.clone(),
            serial_no: c.serial_no.clone(),
        };
        // A SERIAL ONLY RECLAIMS ITS SLOT IF THAT SLOT IS EMPTY. Matching on
        // the serial alone let a second counter reporting the same one --
        // which no two real tubes do, but every synthetic counter does, and
        // one relabelled tube would -- take over the live slot and close the
        // port of the counter already in it. A tube that is answering cannot
        // simultaneously be arriving.
        let free = |k: usize| self.counters.get(k).map(|c| c.is_none()).unwrap_or(false);
        let who = match self
            .ids
            .iter()
            .position(|k| k.serial_no == id.serial_no)
            .filter(|k| free(*k))
        {
            Some(i) => {
                self.ids[i] = id;
                self.counters[i] = Some(std::sync::Arc::new(c));
                i
            }
            None => {
                self.ids.push(id);
                self.counters.push(Some(std::sync::Arc::new(c)));
                self.ids.len() - 1
            }
        };
        if self.running {
            self.spawn(who);
        }
        who
    }

    /// The reader for one slot: samples until the counter goes quiet, then a
    /// single Gone and the thread ends.
    fn spawn(&self, who: usize) {
        let Some(c) = self.counters[who].clone() else { return };
        let tx = self.tx.clone();
        c.heartbeat(true);
        std::thread::spawn(move || {
            loop {
                match c.next_sample(Duration::from_millis(2500)) {
                    Some(v) => {
                        if tx.send(Tube::Sample { who, when: clock::now(), counts: v as u32 })
                            .is_err()
                        {
                            return;
                        }
                    }
                    None => break,
                }
            }
            let _ = tx.send(Tube::Gone { who });
        });
    }

    fn identity(&self, spans: &[f64]) -> broker::Identity {
        broker::Identity { counters: self.ids.clone(), spans: spans.to_vec() }
    }

    fn len(&self) -> usize {
        self.ids.len()
    }

    /// The ports currently held, for a rescan that wants to skip them.
    fn live_paths(&self) -> Vec<String> {
        self.counters
            .iter()
            .filter_map(|c| c.as_ref().map(|c| c.path.clone()))
            .collect()
    }

    fn live(&self) -> Vec<std::sync::Arc<counter::Counter>> {
        self.counters.iter().flatten().cloned().collect()
    }

    /// Begin reading. Nothing arrives before this, and it must not be called
    /// until every port conversation -- the backfill above all -- is over.
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
    /// broken counter.
    fn start(&mut self) {
        if self.running {
            return;
        }
        self.running = true;
        for who in 0..self.counters.len() {
            self.spawn(who);
        }
    }

    /// None means nothing arrived in `timeout` -- NOT that the bank is over.
    /// An empty bank is one waiting for a counter to be plugged in.
    fn next(&self, timeout: Duration) -> Option<Tube> {
        self.rx.recv_timeout(timeout).ok()
    }

    /// Forget a tube that has stopped answering, releasing its port.
    fn retire(&mut self, who: usize) {
        if let Some(slot) = self.counters.get_mut(who) {
            *slot = None;
        }
    }

    fn stop(&self) {
        for c in self.counters.iter().flatten() {
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
    fn counters(&self) -> Vec<std::sync::Arc<counter::Counter>> {
        match self {
            Feed::Own { bank, .. } => bank.live(),
            Feed::Attached(_) => Vec::new(),
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
                match bank.next(timeout)? {
                    Tube::Sample { who, when, counts } => {
                        if let Some(s) = srv.as_mut() {
                            s.publish_sample(who, when, counts);
                        }
                        Some(Tick::Sample { who, when, counts })
                    }
                    // A tube that has gone quiet in the MONITOR is the end of
                    // it: `watch` is a session somebody is sitting in front
                    // of, not a service, and it says so and stops rather than
                    // waiting for a counter to come back.
                    Tube::Gone { .. } => None,
                }
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
                // An attached monitor draws one counter's worth of screen and
                // does not redraw itself for a tube joining mid-session; the
                // samples still arrive and still count. The window is what
                // grows a needle for it.
                broker::Event::Counter { .. } | broker::Event::Gone { .. } => {
                    Some(Tick::Live)
                }
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
    // None when this monitor is attached to a service: the process holding
    // the ports writes both files, and a client that wrote either would be a
    // second hand on the same record. See broker.rs.
    let mut merged_log: Option<MergedLogger> = None;
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
        // And the room they are all in, beside them. See `MergedLogger`.
        let mut m = MergedLogger::new(spans, wl.dir.clone(), wl.every);
        m.counters(
            &feed.counters().iter().map(|c| c.serial_no.clone()).collect::<Vec<_>>(),
        );
        merged_log = Some(m);
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
        (0..tubes).map(|_| new_pool()).collect();
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
                p.add_at(when, counts);
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
        if let Some(m) = merged_log.as_mut() {
            if let Err(e) = m.add(who, when, counts, &w) {
                table_note = format!("NOT LOGGING THE MERGE: {}", e);
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
        // WHOLE SECONDS, AND ONE TIER BELOW THEM FOR THE INTERLEAVE. One tube
        // keeps the strip it always had; see analysis::tiers_interleaved.
        let strip = if tubes > 1 {
            analysis::tiers_interleaved(&series, dropped, width - 1, tubes)
        } else {
            analysis::tiers_with(&series, dropped, width - 1, spec.window, analysis::TIERS, 1.0)
        };
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
                // Before the draw: see the note in `service`.
                let keep = logging.as_ref().map(|w| w.frames).unwrap_or(false);
                let frame = keep.then(|| pools[k].frame(pools[k].seq, suspect));
                let (text, record) = pools[k].draw();
                let at_time = clock::format(clock::now(), "%H:%M:%S");
                let serial = id.counters.get(k).map(|c| c.serial_no.as_str()).unwrap_or("");
                if let Some(wl) = logging.as_ref() {
                    let _ = entropy::write_record(&wl.dir, &record, serial, suspect);
                    if let Some(f) = frame.as_ref() {
                        let _ = entropy::write_frame(&wl.dir, f, serial);
                    }
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
            // THE CLOCK SITS ON THE TABLE, when there is one.
            //
            // It used to be pinned two rows under the random line, which is
            // where it belongs when nothing follows it: it does not jump as
            // the first emission arrives, and a screenshot says when it was
            // taken. But the table is anchored to the BOTTOM of the screen,
            // and on a short one the content above ran out first -- leaving a
            // blank row between the clock and the log rows, with everything
            // else pushed tight. Two things anchored to opposite ends of the
            // same gap is how that happens.
            //
            // So when the table is drawn the clock goes directly above it,
            // which is also where it reads best: the clock says when, and the
            // rows under it say what went to disk at that time. `max` keeps
            // it clear of the random line on a screen tall enough for the
            // content to reach down that far.
            let clock_row = if tl > 0 {
                h.saturating_sub(3 + tl).max(row + 2)
            } else {
                row + 2
            };
            if clock_row < hv.saturating_sub(2) {
                out.push_str(&format!("{}{}clock{}    {}", at(clock_row, 0), DIM, OFF,
                                      clock::format(clock::now(), "%Y-%m-%d %H:%M:%S")));
                // Who else is reading this counter through us. Worth a word
                // because the whole point of the socket is invisible
                // otherwise -- a GUI attaching shows up here and nowhere else.
                match feed.watchers() {
                    0 => {}
                    1 => out.push_str(&format!("{}{}1 attached{}", at(clock_row, 24), DIM, OFF)),
                    n => out.push_str(&format!("{}{}{} attached{}", at(clock_row, 24), DIM, n, OFF)),
                }
                // What the log is doing, beside it: what the backfill found,
                // or that rows are not reaching the disk.
                if !table_note.is_empty() && width > 40 + table_note.len() {
                    let tint = if table_note.starts_with("NOT") { YELLOW } else { DIM };
                    out.push_str(&format!("{}{}{}{}", at(clock_row, 32), tint, table_note, OFF));
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
    if let Some(m) = merged_log.as_mut() {
        m.finish(&w);
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
#[allow(clippy::too_many_arguments)]
fn service(spans: &[f64], every: f64, duration: Option<f64>,
           devices: &[String], baud: Option<u32>,
           logs: Option<std::path::PathBuf>,
           backfill: Option<(usize, f64)>, frames: bool) -> i32 {
    install_stop_handler();

    // WHERE THIS SERVICE'S STATUS BELONGS, settled before anything is written:
    // a service on its own --logs has its own status, and must not report
    // itself into somebody else's.
    let dir = logs.unwrap_or_else(log::state_dir);
    let _ = std::fs::create_dir_all(&dir);

    let started = clock::now();
    let mut waiting = false;
    let found = loop {
        let (found, why) = counter::find_all(devices, baud);
        match why {
            None => break found,
            Some(e) => {
                if !e.busy {
                    let path = log::write_status(&dir, &format!("dormant: {}", e.reason));
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
                    log::write_status(&dir, &format!("waiting: {}", e.reason));
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

    // EVERY COUNTER BACKFILLED FROM ITS OWN FLASH, into its own file. Two
    // tubes were both in the room while nobody was listening and both wrote
    // down what they saw; the records stay apart, because the only way to ask
    // whether two instruments agree is to have kept both their answers.
    if let Some((bytes, max_gap)) = backfill {
        log::write_status(&dir, "backfilling from the counters' history");
        for c in &found {
            println!("radbeeper: {}",
                     backfill_at_start(c, &dir, spans, every, bytes, max_gap, false));
        }
    }

    let tubes = found.len();
    log::write_status(&dir, &format!(
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
    // AND ONE SET ACROSS ALL OF THEM, which is not a contradiction of the
    // note above. The per-counter files stay per-counter; this is the second
    // file, beside them, saying what the room did -- see `MergedLogger`, and
    // note that it can be taken apart again because `per_tube` is in it.
    let mut all = Windows::new(spans);
    let mut merged_log = MergedLogger::new(spans, dir.clone(), every);
    merged_log.counters(
        &id.counters.iter().map(|c| c.serial_no.clone()).collect::<Vec<_>>(),
    );
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
        (0..tubes).map(|_| new_pool()).collect();
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
    if id.counters.len() > 1 {
        println!("radbeeper: the room, merged, to {}",
                 log::path(clock::now(), &dir, Some(log::MERGED)).display());
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
    let mut swept = clock::now();
    loop {
        // Whoever turned up while we were waiting for this second. Before the
        // read, so a window that has just opened is greeted within a second
        // rather than after the next sample.
        if let Some(s) = srv.as_mut() {
            s.accept_pending();
        }
        if stopping() {
            break;
        }
        if duration.map(|d| clock::now() - started >= d).unwrap_or(false) {
            break;
        }
        // ---- A COUNTER PLUGGED INTO A RUNNING SERVICE ------------------
        //
        // Once a log cycle, look for one. Plugging a second tube in used to
        // mean restarting the service, which means a hole in every tube's
        // record to pick up one of them -- and nothing about the fan-out
        // needed it: a counter is a thread, a logger and a line on the wire,
        // and all three can be made while the others are running.
        //
        // The flock does the filtering. A port this process already holds
        // fails to open exactly as another process's would, so every
        // candidate can be tried and the ones already held fall out on their
        // own, with no list to keep and no list to go stale.
        if clock::now() - swept >= RESCAN {
            swept = clock::now();
            let held = bank.live_paths();
            let ports: Vec<String> = if devices.is_empty() {
                counter::candidate_ports()
            } else {
                devices.to_vec()
            };
            for port in ports.into_iter().filter(|p| !held.contains(p)) {
                let Some(c) = counter::open_at(&port, baud) else { continue };
                let (path, version, serial) =
                    (c.path.clone(), c.version.clone(), c.serial_no.clone());
                let who = bank.adopt(c);
                // Its own flash first, exactly as at start: this tube was
                // recording while nobody was listening to it.
                if let Some((bytes, max_gap)) = backfill {
                    if let Some(k) = bank.counters.get(who).and_then(|c| c.clone()) {
                        println!("radbeeper: {}",
                                 backfill_at_start(&k, &dir, spans, every, bytes, max_gap, false));
                    }
                }
                if who == each.len() {
                    each.push(Windows::new(spans));
                    pools.push(new_pool());
                    loggers.push(Logger::new(spans, dir.clone(), &serial, every));
                } else {
                    // A tube that has come back: its windows start again, its
                    // log does not.
                    each[who] = Windows::new(spans);
                }
                let id = bank.identity(spans);
                merged_log.counters(
                    &id.counters.iter().map(|c| c.serial_no.clone()).collect::<Vec<_>>(),
                );
                if let (Some(s), Some(c)) = (srv.as_mut(), id.counters.get(who)) {
                    s.announce(who, c);
                }
                log::write_status(&dir, &format!("monitoring {} ({})", path, version));
                println!("radbeeper: {} joined -- {} ({})", path, version, serial);
            }
        }
        let (who, when, counts) = match bank.next(Duration::from_millis(2500)) {
            Some(Tube::Sample { who, when, counts }) => (who, when, counts),
            // A TUBE GOING QUIET IS NOT THE END OF THE SERVICE any more. Its
            // port is released so it can be taken again, everybody watching
            // is told, and the sweep above will pick it back up when it is
            // plugged in. Nothing is exited: a service that stopped logging
            // the counters still present because one was unplugged would be
            // the worst possible reading of "one counter went away".
            Some(Tube::Gone { who }) => {
                bank.retire(who);
                if let Some(s) = srv.as_mut() {
                    s.publish_gone(who);
                }
                let name = bank.ids.get(who).map(|c| c.path.clone()).unwrap_or_default();
                println!("radbeeper: {} stopped answering -- waiting for it", name);
                log::write_status(&dir, &format!("waiting: {} stopped answering", name));
                continue;
            }
            None => continue,
        };
        each[who].add(when, counts);
        all.add(when, counts);
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
        pools[who].add_at(when, counts);
        // The line, when a pool has earned it: written once, here, against the
        // tube that earned it, and sent to every window so they all show the
        // same hex.
        if pools[who].ready() {
            let spec = ladder.best();
            let (top, _) = spec.loudest();
            let suspect = top > 0.0 && top >= spec.chance_max() * 1.25;
            // THE FRAME IS TAKEN BEFORE THE DRAW, because drawing empties the
            // pool. It is the raw material the key came out of -- every
            // second, unclamped, with the gaps where the counter was away.
            let frame = frames.then(|| pools[who].frame(pools[who].seq, suspect));
            let (text, record) = pools[who].draw();
            let at_time = clock::format(clock::now(), "%H:%M:%S");
            let _ = entropy::write_record(&dir, &record, &id.counters[who].serial_no, suspect);
            if let Some(f) = frame.as_ref() {
                if let Err(e) = entropy::write_frame(&dir, f, &id.counters[who].serial_no) {
                    eprintln!("radbeeper: could not write the frame -- {}", e);
                }
            }
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
        // AND THE MERGE, WHOSE FAILURE IS NOT FATAL. The per-counter files
        // are the record and a service that cannot write them should stop;
        // this one is the second view of the same seconds, and losing it is
        // worth a line on stderr and nothing else.
        if let Err(e) = merged_log.add(who, when, counts, &all) {
            eprintln!("radbeeper: could not write the merged log -- {}", e);
        }
    }
    for (k, lg) in loggers.iter_mut().enumerate() {
        let averages: Vec<Option<f64>> = spans.iter().map(|s| each[k].average(*s)).collect();
        lg.finish(&averages);
    }
    merged_log.finish(&all);
    bank.stop();
    log::write_status(&dir, "stopped");
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

/// Rows to the merged log: the room, rather than any one instrument.
///
/// THE SECOND FILE, AND WHY THERE IS A SECOND FILE. `Logger` above writes the
/// format of record -- one file per counter, the Python's characters to the
/// byte, and a row in it is a reading ONE tube took. Nothing about two tubes
/// belongs in it: a row that blended them could never be taken apart again,
/// and the whole reason to run a second instrument is to be able to ask
/// whether the two agree.
///
/// So the merge is its own file, `cpm-merged-YYYY-MM.tsv`, carrying what the
/// per-counter files cannot say between them -- the combined rate, its error
/// bar, the interleave, and the raw arrivals that produced all three -- at
/// the precision that reads back bit-identical. See `log::merged_header`.
///
/// ONE TUBE WRITES NO MERGED FILE. There is nothing to merge and nothing to
/// interleave: `per_tube` would have a single entry, `interleave` would be
/// empty, and `cps` would be the counter's own rate restated to more places.
/// A second file that only ever repeats the first is a second file to explain,
/// to back up and to get out of step. So the merge begins when there is
/// something to merge -- including when a second counter is plugged into a
/// running service, which is exactly when a merged record starts being worth
/// keeping.
struct MergedLogger {
    dir: PathBuf,
    spans: Vec<f64>,
    every: f64,
    out: log::Writer,
    serials: Vec<String>,
    /// The interval: every arrival off every tube, and the tube-time behind
    /// them.
    counts: u64,
    per_tube: Vec<u64>,
    tube_seconds: f64,
    /// WHICH WALL SECONDS THIS ROW COVERS, as a set of them.
    ///
    /// NOT A COUNT OF SAMPLES, which with n tubes is n per second, and not
    /// the span from first to last, which is short by however long the last
    /// sample lasted. Two tubes both reporting second 1,203 make that one
    /// second of room however many samples it took, and the set says so
    /// exactly however the samples happen to arrive.
    wall: BTreeSet<i64>,
    /// WHICH TUBES ACTUALLY REPORTED IN THIS INTERVAL.
    ///
    /// NOT THE NUMBER OF SLOTS, which only ever grows: a tube that is
    /// unplugged keeps its slot -- deliberately, so that plugging it back in
    /// returns it to its own colour and its own log -- and a divisor taken
    /// from the slot count would go on dividing the room by a counter that
    /// left the building. The set is emptied with the rest of the interval,
    /// so it answers for the thirty seconds the row is about.
    seen: BTreeSet<usize>,
    peaks: Vec<Option<f64>>,
    /// The interleave, measured over this interval only: the mean gap between
    /// one tube's sample and the NEXT TUBE'S. Same-tube gaps are a second by
    /// definition and would drown it.
    prev: Option<(usize, f64)>,
    gap: (f64, u32),
    due: Option<f64>,
    sites: Vec<(String, f64, String)>,
    sites_mtime: Option<u64>,
}

impl MergedLogger {
    fn new(spans: &[f64], dir: PathBuf, every: f64) -> MergedLogger {
        let sites = log::read_sites(&dir);
        let sites_mtime = mtime_of(&dir.join("sites.tsv"));
        MergedLogger {
            out: log::Writer::merged(spans, dir.clone(), every),
            spans: spans.to_vec(),
            dir,
            every,
            serials: Vec::new(),
            counts: 0,
            per_tube: Vec::new(),
            tube_seconds: 0.0,
            wall: BTreeSet::new(),
            seen: BTreeSet::new(),
            peaks: vec![None; spans.len()],
            prev: None,
            gap: (0.0, 0),
            due: None,
            sites,
            sites_mtime,
        }
    }

    /// The tubes as they stand, so a counter plugged into a running service
    /// gets a column in the next row rather than at the next restart.
    fn counters(&mut self, serials: &[String]) {
        self.serials = serials.to_vec();
        self.per_tube.resize(serials.len(), 0);
    }

    /// One sample, and the combined windows as they stand after it.
    ///
    /// Returns the row when this sample completed an interval, exactly as
    /// `Logger::add` does.
    fn add(&mut self, who: usize, when: f64, counts: u32, all: &Windows)
        -> std::io::Result<Option<String>>
    {
        if !self.worth_keeping() {
            // Whatever one tube put in the interval is not the start of a
            // merged row. Dropped rather than carried, so that the first row
            // after a second counter joins is a row about two counters.
            self.reset();
            self.due = None;
            return Ok(None);
        }
        self.seen.insert(who);
        self.counts += counts as u64;
        if self.per_tube.len() <= who {
            self.per_tube.resize(who + 1, 0);
        }
        self.per_tube[who] += counts as u64;
        self.tube_seconds += 1.0;
        self.wall.insert(when.floor() as i64);
        let tubes = self.tubes();
        if let Some((prev, t)) = self.prev {
            if prev != who && when > t {
                self.gap.0 += when - t;
                self.gap.1 += 1;
            }
        }
        self.prev = Some((who, when));
        for (i, span) in self.spans.iter().enumerate() {
            if let Some(v) = mean_of(all, *span, tubes) {
                let slot = &mut self.peaks[i];
                if slot.is_none() || v > slot.unwrap() {
                    *slot = Some(v);
                }
            }
        }
        let due = *self.due.get_or_insert(when + self.every);
        if when < due {
            return Ok(None);
        }
        let line = self.row(all);
        let wrote = self.out.write(clock::now(), &line)?;
        self.reset();
        self.due = Some(when + self.every);
        Ok(wrote.then_some(line))
    }

    /// Whatever the last interval collected, as `Logger::finish` does: a
    /// service stopped four seconds after a spike leaves the spike on disk.
    /// Whether there is a room to write down, rather than one instrument.
    fn worth_keeping(&self) -> bool {
        self.serials.len() > 1
    }

    fn finish(&mut self, all: &Windows) {
        if self.worth_keeping() && !self.wall.is_empty() {
            let line = self.row(all);
            let _ = self.out.write(clock::now(), &line);
        }
        self.out.close();
    }

    fn reset(&mut self) {
        self.counts = 0;
        for c in self.per_tube.iter_mut() {
            *c = 0;
        }
        self.tube_seconds = 0.0;
        self.wall.clear();
        self.seen.clear();
        for p in self.peaks.iter_mut() {
            *p = None;
        }
        self.gap = (0.0, 0);
    }

    /// How many tubes this interval is the mean of.
    ///
    /// The combined windows hold every tube's samples, so their sums are
    /// `tubes` times the room and the room is that over `tubes`. Over a
    /// thirty-thousand-second window the count may have changed, and nothing
    /// here records when -- so this is the tubes reporting NOW, which is the
    /// best available answer and the one the panels already use. The `tubes`
    /// column carries it, so a reader can see what the division was.
    fn tubes(&self) -> usize {
        if self.seen.is_empty() {
            // Nothing has reported yet, so the plugged-in count is all there
            // is to go on.
            self.serials.len().max(1)
        } else {
            self.seen.len()
        }
    }

    fn row(&mut self, all: &Windows) -> String {
        let now = clock::now();
        let tubes = self.tubes();
        let m = mtime_of(&self.dir.join("sites.tsv"));
        if m != self.sites_mtime {
            self.sites_mtime = m;
            self.sites = log::read_sites(&self.dir);
        }
        // THE SITE OF THE MERGE IS THE SITE OF THE FIRST TUBE. They are in one
        // room -- that is the premise the merge rests on -- and if they are
        // not, `together` in the report is the thing that says so.
        let site = self
            .serials
            .first()
            .and_then(|s| log::site_at(s, now, &self.sites))
            .unwrap_or_default();
        let averages: Vec<Option<f64>> =
            self.spans.iter().map(|s| mean_of(all, *s, tubes)).collect();
        let sigma = mean_of(all, HEADLINE_SPAN, tubes)
            .and_then(|v| sigma_of(v, HEADLINE_SPAN, tubes));
        let per_tube: Vec<(String, u64)> = self
            .per_tube
            .iter()
            .enumerate()
            .map(|(k, n)| {
                let name = self
                    .serials
                    .get(k)
                    .cloned()
                    .unwrap_or_else(|| format!("tube{}", k));
                (name, *n)
            })
            .collect();
        log::merged_row(
            now,
            tubes,
            (self.gap.1 > 0).then(|| self.gap.0 / self.gap.1 as f64),
            self.counts,
            self.wall.len() as f64,
            self.tube_seconds,
            &averages,
            sigma,
            &self.peaks,
            &per_tube,
            log::SRC_LIVE,
            &site,
        )
    }
}

/// The window the merged log's error bar is quoted over.
///
/// The same thirty seconds the panels put in their big number, so the figure
/// on disk and the figure on the screen are the same figure.
const HEADLINE_SPAN: f64 = 30.0;

/// The mean rate across the tubes, from windows holding every tube's samples.
///
/// TWO TUBES ARE TWO MEASUREMENTS OF ONE NUMBER. They do not double the dose;
/// they double the evidence. The combined windows hold every tube's samples,
/// so their sum is n times the room and the room is that over n.
fn mean_of(w: &Windows, span: f64, tubes: usize) -> Option<f64> {
    w.average(span).map(|v| v / tubes.max(1) as f64)
}

/// One sigma on that mean, in CPM.
///
/// Arrivals are Poisson, so the whole of the uncertainty is the count behind
/// the number: N arrivals give a relative error of 1/sqrt(N), and the counts
/// behind a mean of `tubes` tubes over `span` seconds is `cpm * span * tubes
/// / 60`. THIS is what a second counter buys -- the same figure, known to
/// within a factor of root two better -- and recording it is the only way the
/// benefit survives into the record.
fn sigma_of(cpm: f64, span: f64, tubes: usize) -> Option<f64> {
    let n = cpm * span * tubes.max(1) as f64 / 60.0;
    (n > 0.0).then(|| cpm / n.sqrt())
}

fn mtime_of(path: &std::path::Path) -> Option<u64> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| m.mtime() as u64)
}

/// How many bits an emission is worth, when it is not the usual 256.
///
/// A ONCE-SET GLOBAL, read where a pool is built. The pools are created in
/// three places -- the service, the monitor, and the sweep that adopts a tube
/// plugged in while either is running -- and threading a float through all of
/// them to serve one flag nobody sets in ordinary use is more moving parts
/// than the flag is worth. `--entropy-bits` was in the reference program and
/// in this program's own `--help` for a year without being implemented here,
/// which is the other half of why it is being added rather than deleted.
static WANT_BITS: std::sync::OnceLock<f64> = std::sync::OnceLock::new();

fn new_pool() -> entropy::Entropy {
    entropy::Entropy::new(*WANT_BITS.get().unwrap_or(&entropy::ENTROPY_BITS))
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
/// `random --frames FILE`: what is in a frame file, and does it hold up.
///
/// THE FRAME IS THE EVIDENCE AND THIS IS HOW YOU LOOK AT IT. The `.tsv`
/// carries the counts clamped to one hex digit a second, which is what the
/// key is derived from and is not what the tube did; the `.bin` carries every
/// second exactly, with the gaps where the counter was away. Each frame
/// recomputes its own key from its own counts, so a file from somebody else's
/// machine can be audited without trusting anything but the arithmetic.
fn frames_report(path: &std::path::Path) -> i32 {
    let frames = entropy::read_frames(path);
    if frames.is_empty() {
        eprintln!("radbeeper: no frames in {}", path.display());
        return 1;
    }
    let bytes = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let mut bad = 0;
    let mut samples = 0usize;
    for f in &frames {
        let counts = f.counts();
        let ok = f.verifies();
        if !ok {
            bad += 1;
        }
        samples += counts.len();
        // The gaps are the thing the counts alone cannot say: a pool that
        // spanned an unplugged counter looks exactly like one that did not.
        let holes = f.samples.iter().skip(1).filter(|(g, _)| *g != 1).count();
        println!(
            "  seq {:<4} {:<20} {:>5}s  {:>5} samples  peak {:>5}  {:.3} bits/s  {}{}",
            f.seq,
            clock::stamp(f.started as f64),
            f.seconds(),
            counts.len(),
            counts.iter().copied().max().unwrap_or(0),
            entropy::mcv_min_entropy(&counts),
            if ok { "recomputes" } else { "DOES NOT RECOMPUTE" },
            if holes == 0 {
                String::new()
            } else {
                format!("  ({} gap{})", holes, if holes == 1 { "" } else { "s" })
            }
        );
        if f.suspect {
            println!("       spectrum was not flat when this was drawn -- suspect");
        }
    }
    println!(
        "radbeeper: {} of {} frames recompute from their own seconds",
        frames.len() - bad,
        frames.len()
    );
    println!(
        "radbeeper: {} seconds of counts in {} bytes -- {:.2} bytes a second",
        samples,
        bytes,
        if samples > 0 { bytes as f64 / samples as f64 } else { 0.0 }
    );
    if bad > 0 { 1 } else { 0 }
}

/// `frames <verb>`: the raw record, from the command line.
///
/// THE SAME ANSWERS THE PAGE GIVES, WITHOUT A BROWSER. `frames.html` is the
/// comfortable way to read this and a terminal is the one that is always
/// there -- over ssh, in a service's log, on a machine with no display at
/// all -- so every view the page has is a verb here as well, and both of them
/// read the files through `radbeeper::frames` rather than each other.
fn frames_cmd(action: &str, dir: &std::path::Path, serial: Option<&str>,
              month: Option<&str>, seq: Option<u64>, form: &str,
              output: Option<&str>, file: Option<&std::path::Path>) -> i32 {
    use radbeeper::frames as api;

    // WHICH COUNTER, AND SAYING SO RATHER THAN GUESSING. One counter is the
    // ordinary case and naming it every time would be noise; two is the case
    // this machine is actually in, and picking one of them silently is how a
    // person ends up auditing the wrong tube.
    let found = api::counters(dir);
    let chosen: Vec<String> = match serial {
        Some(s) => vec![s.to_string()],
        None if found.len() == 1 => found.clone(),
        None if found.is_empty() => {
            eprintln!("radbeeper: no frame files in {}", dir.display());
            eprintln!("    frames are written by `radbeeper service` and `radbeeper watch`,");
            eprintln!("    as random-<serial>-YYYY-MM.bin, unless --no-frames is given.");
            return 1;
        }
        None if action == "list" || action == "verify" => found.clone(),
        None => {
            eprintln!("radbeeper: {} counters here -- name one with --serial:", found.len());
            for s in &found {
                eprintln!("    {}", s);
            }
            return 1;
        }
    };

    match action {
        "list" => {
            for s in &chosen {
                let series = api::Series::open(dir, s);
                println!("counter {}", s);
                if series.months().is_empty() {
                    println!("  no frames");
                    continue;
                }
                let mut samples = 0usize;
                for m in series.months() {
                    let frames = series.read(&m.label);
                    let secs: usize = frames.iter().map(|f| f.samples.len()).sum();
                    samples += secs;
                    println!(
                        "  {:<9} {:>6} frames  {:>9}  {:>9} seconds  {} .. {}",
                        if m.label.is_empty() { "undated" } else { &m.label },
                        m.frames,
                        bytes_text(m.bytes),
                        secs,
                        clock::format(m.first as f64, "%Y-%m-%d"),
                        clock::format(m.last as f64, "%Y-%m-%d"),
                    );
                }
                let bytes = series.bytes();
                println!(
                    "  {} frames, {} seconds, {} -- {:.2} bytes a second",
                    series.frames(), samples, bytes_text(bytes),
                    if samples > 0 { bytes as f64 / samples as f64 } else { 0.0 }
                );
                println!("  chain head {}", api::head(dir, s));
            }
            0
        }
        "verify" => {
            let mut bad = 0;
            for s in &chosen {
                let series = api::Series::open(dir, s);
                let frames = series.read_all();
                if frames.is_empty() {
                    println!("counter {}: no frames", s);
                    continue;
                }
                let v = api::verify(&frames, entropy::GENESIS_LINK);
                println!(
                    "counter {}: {} of {} recompute, {} of {} link{}",
                    s, v.keys_ok, v.frames, v.links_ok,
                    v.frames - v.unlinked,
                    if v.unlinked > 0 {
                        format!(", {} unlinked (written before the chain)", v.unlinked)
                    } else {
                        String::new()
                    }
                );
                for seq in &v.broken {
                    println!("  seq {} does not hold up", seq);
                }
                // THE HEAD IS CHECKED AGAINST THE DISK, not just recomputed.
                // A chain that verifies internally but does not end where the
                // next frame will be linked from is a fork waiting to happen.
                let disk = api::head(dir, s);
                if v.head != disk {
                    println!("  chain head disagrees with the file:");
                    println!("    recomputed {}", v.head);
                    println!("    on disk    {}", disk);
                    bad += 1;
                }
                if !v.broken.is_empty() {
                    bad += 1;
                }
            }
            if bad > 0 { 1 } else { 0 }
        }
        "show" => {
            let s = &chosen[0];
            let series = api::Series::open(dir, s);
            let frames = match month {
                Some(m) => series.read(m),
                None => series.read_all(),
            };
            if frames.is_empty() {
                eprintln!("radbeeper: no frames for {}{}", s,
                          month.map(|m| format!(" in {}", m)).unwrap_or_default());
                return 1;
            }
            let picked: Vec<&entropy::Frame> = match seq {
                Some(n) => frames.iter().filter(|f| f.seq == n).collect(),
                None => frames.last().into_iter().collect(),
            };
            if picked.is_empty() {
                eprintln!("radbeeper: no frame with seq {} for {}", seq.unwrap_or(0), s);
                eprintln!("    `seq` belongs to the pool and restarts; \
                           `radbeeper frames list` has the months.");
                return 1;
            }
            // seq RESTARTS, so asking for one can legitimately find several.
            // Showing the first and saying nothing would be answering a
            // different question than the one asked.
            if picked.len() > 1 {
                eprintln!("radbeeper: {} frames carry seq {} -- narrow it with --month:",
                          picked.len(), seq.unwrap_or(0));
                for f in &picked {
                    eprintln!("    {}  {}", clock::stamp(f.started as f64),
                              clock::format(f.started as f64, "%Y-%m"));
                }
                return 1;
            }
            inspect(picked[0], s);
            0
        }
        "export" => {
            let s = &chosen[0];
            let series = api::Series::open(dir, s);
            let frames = match month {
                Some(m) => series.read(m),
                None => series.read_all(),
            };
            if frames.is_empty() {
                eprintln!("radbeeper: no frames to export for {}", s);
                return 1;
            }
            let head = vec![
                format!("radbeeper {} -- counter {}", VERSION, s),
                format!("frames {}{}", frames.len(),
                        month.map(|m| format!(" from {}", m)).unwrap_or_default()),
            ];
            let (bytes, what): (Vec<u8>, &str) = match form {
                "bin" => (api::to_bytes(&frames), "bytes"),
                "tsv" => (api::to_tsv(&frames, &head).into_bytes(), "rows"),
                _ => (api::to_json(&frames).into_bytes(), "json"),
            };
            match output {
                Some(path) => {
                    if let Err(e) = std::fs::write(path, &bytes) {
                        eprintln!("radbeeper: {}: {}", path, e);
                        return 1;
                    }
                    println!("radbeeper: {} -- {} frames, {} of {}",
                             path, frames.len(), bytes_text(bytes.len() as u64), what);
                }
                None if form == "bin" => {
                    // Frame bytes contain every byte value there is, and a
                    // terminal is not a file. `-o` is not a convenience here.
                    eprintln!("radbeeper: --bin writes bytes -- give it -o FILE");
                    return 2;
                }
                None => {
                    print!("{}", String::from_utf8_lossy(&bytes));
                }
            }
            0
        }
        "import" => {
            let s = &chosen[0];
            let Some(path) = file else {
                eprintln!("radbeeper: frames import needs a file (.bin or .json)");
                return 2;
            };
            let frames = match api::read_any(path) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("radbeeper: {}", e);
                    return 1;
                }
            };
            match api::import(dir, s, &frames) {
                Ok(done) => {
                    println!("radbeeper: {} of {} frames written to {}",
                             done.written, done.offered, dir.display());
                    for p in &done.files {
                        println!("  {}", p.display());
                    }
                    if done.duplicates > 0 {
                        println!("  {} already here, by key -- not written again",
                                 done.duplicates);
                    }
                    for seq in &done.refused {
                        println!("  seq {} does not produce its own key -- REFUSED", seq);
                    }
                    println!("  chain head {}", api::head(dir, s));
                    if done.refused.is_empty() { 0 } else { 1 }
                }
                Err(e) => {
                    eprintln!("radbeeper: import: {}", e);
                    1
                }
            }
        }
        other => {
            eprintln!("radbeeper: frames {}? -- list, show, export, import, verify", other);
            2
        }
    }
}

/// One frame, in full: what it is, what it sounds like, and every second.
///
/// THE INSPECTOR'S JOB IS TO SHOW THE EVIDENCE, NOT A SUMMARY OF IT. The key
/// is a claim about a particular stretch of decay; the seconds below are that
/// stretch, unclamped, with the gaps where the counter was away marked -- so
/// the line that says `recomputes` can be believed by someone who does the
/// arithmetic themselves.
fn inspect(f: &entropy::Frame, serial: &str) {
    use radbeeper::frames as api;
    let counts = f.counts();
    let total: u64 = counts.iter().map(|c| *c as u64).sum();
    let secs = f.seconds().max(1);
    println!("counter    {}", serial);
    println!("seq        {}", f.seq);
    println!("started    {}", clock::stamp(f.started as f64));
    println!("covers     {} seconds in {} samples", f.seconds(), counts.len());
    println!("counts     {}, {:.2} a second, peak {}",
             total, total as f64 / secs as f64, counts.iter().copied().max().unwrap_or(0));
    println!("min-entropy {:.3} bits a sample (most-common-value)",
             entropy::mcv_min_entropy(&counts));
    println!("key        {}", f.key);
    println!("link       {}", if f.link.is_empty() { "-- (written before the chain)" } else { &f.link });
    println!("recomputes {}", if f.verifies() { "yes" } else { "NO -- this frame does not produce its key" });

    // The gaps, because the counts alone cannot say the counter was away.
    //
    // A GAP AND A DOUBLED SECOND ARE NOT THE SAME DEPARTURE. Anything that
    // is not one second is irregular, but a gap of four means the counter
    // was away for three seconds and a gap of zero means two samples landed
    // inside one second -- the opposite complaint. Calling both "the counter
    // was away" is the kind of wrong label somebody reasons from later.
    let holes: Vec<(usize, u32)> = f.samples.iter().enumerate().skip(1)
        .filter(|(_, (g, _))| *g > 1).map(|(i, (g, _))| (i, *g)).collect();
    let doubled = f.samples.iter().skip(1).filter(|(g, _)| *g == 0).count();
    if holes.is_empty() {
        println!("gaps       none -- every sample is one second after the last");
    } else {
        println!("gaps       {}", holes.len());
        for (i, g) in holes.iter().take(8) {
            println!("             at sample {}: {} seconds away", i, g);
        }
        if holes.len() > 8 {
            println!("             ... and {} more", holes.len() - 8);
        }
    }
    if doubled > 0 {
        println!("doubled    {} sample{} landed inside a second already sampled",
                 doubled, if doubled == 1 { "" } else { "s" });
    }

    match api::spectrum(&counts) {
        None => println!("\nspectrum   too few seconds to say anything"),
        Some(spec) => {
            println!("\nspectrum   {} second window, {} averaged, flat is 1.0",
                     spec.window, spec.runs);
            let width = 60usize;
            let cols = analysis::spectrum_columns(&spec.relative, width);
            for row in analysis::bar_rows(&cols, width, 6) {
                println!("  {}", row.into_iter().collect::<String>());
            }
            println!("  loudest {:.2} at a period of {:.0}s -- luck alone reaches {:.2}",
                     spec.loudest.0, spec.period, spec.chance_max);
            println!("  {}", if spec.suspect {
                "NOT FLAT: something periodic is in this frame -- treat it as suspect"
            } else {
                "flat: this looks like decay"
            });
            if spec.suspect != f.suspect {
                // NOT A CONTRADICTION. The flag came off the monitor's
                // running ladder -- every second that process had seen, and
                // the sum across both tubes when two were plugged in. This
                // is the ladder over this frame alone. A period longer than
                // the frame cannot appear here and is plain there.
                println!("  the recorder wrote suspect={}, and this frame on its own \
                          recomputes to {}", f.suspect, spec.suspect);
                println!("  (the flag is drawn from the whole run, not from one frame)");
            }
        }
    }

    // Every second, as the digits the record keeps them in. Sixty to a line,
    // so a minute is a line and a pattern has somewhere to show itself.
    //
    // ONE CHARACTER A SECOND STOPS MEANING ANYTHING ABOVE 35, which is a
    // counter on a real source rather than on a desk. Rendering every one of
    // those seconds as the same `+` is a blank reading, not a compressed
    // one, so past that the numbers are written out. The page does the same.
    let peak = counts.iter().copied().max().unwrap_or(0);
    if peak > 35 {
        println!("\nseconds    (counts, ten to a line, ... is a gap)");
        let mut row: Vec<String> = Vec::new();
        for (i, (gap, count)) in f.samples.iter().enumerate() {
            if i > 0 && *gap > 1 {
                row.push("...".to_string());
            }
            row.push(format!("{:>4}", count));
            if row.len() >= 10 {
                println!("  {}", row.join(" "));
                row.clear();
            }
        }
        if !row.is_empty() {
            println!("  {}", row.join(" "));
        }
        return;
    }
    println!("\nseconds    (counts, one character a second, . is a gap)");
    let mut line = String::new();
    let mut at = 0usize;
    for (i, (gap, count)) in f.samples.iter().enumerate() {
        if i > 0 && *gap > 1 {
            for _ in 0..(*gap).min(6).saturating_sub(1) {
                line.push('.');
                at += 1;
                if at % 60 == 0 {
                    println!("  {}", line);
                    line.clear();
                }
            }
        }
        line.push(match count {
            0..=9 => char::from(b'0' + *count as u8),
            10..=35 => char::from(b'a' + (*count - 10) as u8),
            _ => '+',
        });
        at += 1;
        if at % 60 == 0 {
            println!("  {}", line);
            line.clear();
        }
    }
    if !line.is_empty() {
        println!("  {}", line);
    }
}

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

/// A size with an optional K/M/G suffix, as `--frame-budget` takes it.
///
/// `0` is meaningful and is not an error: it asks for a page that embeds no
/// frames at all, which is what somebody publishing a very long record and
/// linking the files instead actually wants.
fn parse_bytes(text: &str) -> Option<u64> {
    let t = text.trim();
    let (digits, scale) = match t.chars().last() {
        Some('k') | Some('K') => (&t[..t.len() - 1], 1024u64),
        Some('m') | Some('M') => (&t[..t.len() - 1], 1024 * 1024),
        Some('g') | Some('G') => (&t[..t.len() - 1], 1024 * 1024 * 1024),
        _ => (t, 1),
    };
    digits.trim().parse::<u64>().ok().map(|n| n.saturating_mul(scale))
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
    println!("  radbeeper random --frames F  the raw seconds behind those lines");
    println!("  radbeeper backfill         fill the log's gaps from the counter's flash");
    println!("  radbeeper log info|pull    what history it holds, or download it");
    println!("  radbeeper frames list      the raw frames on disk, by month");
    println!("  radbeeper frames show      one frame: its seconds, its spectrum, its key");
    println!("  radbeeper frames export    those frames out, as bytes, a table or json");
    println!("  radbeeper frames import F  frames in, recomputed before they are written");
    println!("  radbeeper frames verify    every key and every chain link, from genesis");
    println!("  radbeeper export           index.html and random.html, from the logs");
    println!("  radbeeper pages            the landing page and lab reports, from the documents");
    println!("  radbeeper hotplug          sit in the session, open the monitor on plug-in");
    println!("  radbeeper preview          serve a generated site on loopback, so the viewer can fetch");
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
    println!("      --no-frames            do not keep the raw seconds behind each random line");
    println!("      --no-backfill          skip reading the counter's history at start");
    println!("      --image FILE           backfill from a saved .bin, no counter");
    println!("      --serial SERIAL        which counter an image came from");
    println!("      --bytes N              how much flash to read");
    println!("      --max-gap SECONDS      a longer hole ends the averages");
    println!("      --entropy-bits N       bits a random line is worth (default 256)");
    println!("      --poll SECONDS         hotplug: how often /dev is read (default 4)");
    println!("      --settle SECONDS       hotplug: grace before a new node is opened (default 2)");
    println!("      --port N               preview: the port it listens on (default 8765)");
    println!("      --bind ADDR            preview: the address (default 127.0.0.1)");
    println!("      --tries N              hotplug: attempts per plug event (default 3)");
    println!("      --tui                  hotplug: a terminal, even where radbeeper-gui is installed");
    println!("  -o, --output STEM          where log pull writes .bin and .csv");
    println!("  -o, --output FILE          export: where the page goes (default index.html)");
    println!("      --title TEXT           export: the page's heading");
    println!("      --random-output FILE   export: the audit page (default random.html beside it)");
    println!("      --frame-budget SIZE    export: raw frame bytes carried in the page (default 2M, 0 for none)");
    println!("      --frames-page          export: also write frames.html, the frame browser (Rust only)");
    println!("      --frames-output FILE   export: where the frame browser goes (default frames.html beside it)");
    println!("      --month YYYY-MM        frames: one month rather than all of them");
    println!("      --seq N                frames show: which frame (default the newest)");
    println!("      --json | --tsv | --bin frames export: which form (default json)");
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
    let mut no_frames = false;
    let mut frames_file: Option<std::path::PathBuf> = None;
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
    // `preview`'s two. Loopback by default: a log directory is not something
    // to put on the network by accident, so a wider bind has to be typed.
    let mut port: u16 = 8765;
    let mut bind = "127.0.0.1".to_string();
    let mut log_action = "info".to_string();
    // `frames` and its verb, which is a noun-then-verb command like `log`.
    let mut frames_action = "list".to_string();
    let mut frames_month: Option<String> = None;
    let mut frames_seq: Option<u64> = None;
    let mut frames_form = "json".to_string();
    let mut frames_in: Option<PathBuf> = None;
    let mut frames_page = false;
    let mut frames_output: Option<PathBuf> = None;
    let mut serial: Option<String> = None;
    let mut title = radbeeper::export::DEFAULT_TITLE.to_string();
    let mut random_output: Option<PathBuf> = None;
    let mut no_random_page = false;
    let mut frame_budget = radbeeper::audit::DEFAULT_FRAME_BUDGET;

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
            "--port" => port = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(port),
            "--bind" => bind = next(&mut i).unwrap_or(bind),
            "--log-every" => {
                log_every = next(&mut i).and_then(|v| v.parse().ok()).unwrap_or(log_every)
            }
            "--check" => check = next(&mut i).map(std::path::PathBuf::from),
            "--frames" => frames_file = next(&mut i).map(std::path::PathBuf::from),
            "--image" => image = next(&mut i).map(std::path::PathBuf::from),
            "--serial" => serial = next(&mut i),
            "--bytes" | "--backfill-bytes" => bytes = next(&mut i).and_then(|v| v.parse().ok()),
            "--no-backfill" => no_backfill = true,
            "--no-log" => no_log = true,
            "--no-export" => no_export = true,
            "--no-frames" => no_frames = true,
            "--entropy-bits" => {
                if let Some(v) = next(&mut i).and_then(|v| v.parse::<f64>().ok()) {
                    if v > 0.0 {
                        let _ = WANT_BITS.set(v);
                    }
                }
            }
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
            "--frame-budget" => {
                frame_budget = next(&mut i)
                    .and_then(|v| parse_bytes(&v))
                    .unwrap_or(frame_budget)
            }
            "info" | "pull" if command == "log" => log_action = a.to_string(),
            // A VERB IS ONLY A VERB AFTER ITS NOUN. `export` is a command in
            // its own right; `frames export` is a different thing entirely,
            // and the guard is what keeps the two apart.
            "list" | "show" | "export" | "import" | "verify" if command == "frames" => {
                frames_action = a.to_string()
            }
            "--month" => frames_month = next(&mut i),
            "--seq" => frames_seq = next(&mut i).and_then(|v| v.parse().ok()),
            "--json" => frames_form = "json".to_string(),
            "--tsv" => frames_form = "tsv".to_string(),
            "--bin" => frames_form = "bin".to_string(),
            "--frames-page" => frames_page = true,
            "--frames-output" => frames_output = next(&mut i).map(PathBuf::from),
            _ if command == "frames"
                && frames_action == "import"
                && frames_in.is_none()
                && !a.starts_with('-') =>
            {
                frames_in = Some(PathBuf::from(a))
            }
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
    if command == "frames" {
        std::process::exit(frames_cmd(
            &frames_action,
            &logs.clone().unwrap_or_else(log::state_dir),
            serial.as_deref(),
            frames_month.as_deref(),
            frames_seq,
            &frames_form,
            output.as_deref(),
            frames_in.as_deref(),
        ));
    }
    if command == "export" {
        let dir = logs.unwrap_or_else(log::state_dir);
        let out = PathBuf::from(output.as_deref().unwrap_or("index.html"));
        let code = radbeeper::export::run(
            &dir, &out, !no_random_page, cpm_per_usvh, &title,
            random_output.as_deref(), frame_budget,
        );
        // THE FRAME PAGE IS ASKED FOR, NEVER ASSUMED. Without the flag this
        // command prints and writes exactly what the Python prints and
        // writes for the same arguments, which is what the differential
        // suite compares -- and what a workflow with no Rust toolchain has
        // to be able to reproduce. See the head of src/browser.rs.
        if code == 0 && frames_page {
            match radbeeper::browser::write_pages(
                &dir, &out, &title, frame_budget, frames_output.as_deref(),
            ) {
                Ok(pages) if pages.is_empty() => {
                    println!("radbeeper: no frames in {} -- no frame page written",
                             dir.display());
                    println!("    frames are the .bin files `service` and `watch` \
                              write beside the log.");
                }
                Ok(pages) => {
                    for p in pages {
                        println!("radbeeper: {} -- {} frames in {} month{}",
                                 p.path.display(), p.frames, p.months,
                                 if p.months == 1 { "" } else { "s" });
                    }
                }
                Err(e) => {
                    eprintln!("radbeeper: frames page: {}", e);
                    std::process::exit(1);
                }
            }
        }
        std::process::exit(code);
    }
    if command == "log" {
        std::process::exit(log_cmd(&log_action, bytes, output.as_deref(),
                                   devices.first().map(|s| s.as_str()), baud));
    }
    if command == "random" {
        // --check needs no hardware, so it runs before anything looks for a
        // counter: an audit of a file from another machine is the ordinary
        // case, not an odd one.
        if let Some(p) = frames_file {
            std::process::exit(frames_report(&p));
        }
        if let Some(p) = check {
            std::process::exit(check_random(&p));
        }
        std::process::exit(random(&spans, duration, devices.first().map(|s| s.as_str()), baud, logs));
    }
    // `pages`, NOT `site`. `radbeeper site` is already a command and it means
    // where the COUNTER is -- a place name against a serial over time. This
    // one builds the GitHub Pages site, and two commands one letter apart
    // meaning entirely different things is how somebody ends up publishing a
    // web page when they meant to record a garage.
    if command == "pages" {
        // Built from the documents where they are, so the root is the
        // checkout rather than a log directory: `-o DIR` moves it.
        let root = output.as_deref().map(std::path::Path::new);
        std::process::exit(radbeeper::site::run(root));
    }
    if command == "preview" {
        // `-o DIR` names what to serve, the same way `pages` uses it to name
        // where to write. The default is here, because that is where `make
        // site` has just put a page.
        let root = output.as_deref().map(std::path::Path::new)
            .unwrap_or_else(|| std::path::Path::new("."));
        std::process::exit(radbeeper::serve::run(root, &bind, port));
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
            !no_frames,
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
            frames: !no_frames,
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

    /// The attempt found the port held and nothing serving yet. That is not a
    /// failed try -- the holder is starting up, and a backfill can take many
    /// minutes -- so the try is given back and the port is asked again after
    /// `hold`, for as long as it stays that way. A busy open is refused at the
    /// flock, before the line is configured or written, so asking costs the
    /// holder nothing.
    fn not_yet(&mut self, now: f64, hold: f64) {
        self.tries = (self.tries + 1).min(self.max_tries);
        self.due = now + hold;
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
///
/// A BUSY PORT WITH NO SOCKET IS `NotYet`, not no. Whoever holds the port has
/// not started serving: the service backfills from both flashes before it
/// binds the socket, and that took sixteen minutes on the morning this was
/// written. Three tries a few seconds apart all landed inside it, the plug
/// event was written off, and the login that should have opened the monitor
/// opened nothing.
#[derive(Debug, PartialEq)]
enum Worth {
    Yes,
    No,
    NotYet,
}

fn worth_a_window(dir: &Path, device: Option<&str>, baud: Option<u32>) -> Worth {
    if broker::socket_path(dir).exists() {
        return Worth::Yes;
    }
    match counter::find(device, baud) {
        Ok(_) => Worth::Yes,
        Err(e) if e.busy => Worth::NotYet,
        Err(e) => {
            log::write_status(dir, &format!("dormant: {}", e.reason));
            Worth::No
        }
    }
}

/// What one attempt to open the monitor came to.
enum Opened {
    Window(Child),
    Nothing,
    NotYet,
}

fn open_window(dir: &Path, device: Option<&str>, baud: Option<u32>, tui: bool) -> Opened {
    match worth_a_window(dir, device, baud) {
        Worth::Yes => {}
        Worth::No => return Opened::Nothing,
        Worth::NotYet => return Opened::NotYet,
    }
    // THE WINDOW A PERSON WOULD HAVE OPENED THEMSELVES. If radbeeper-gui is
    // installed, that is a window on its own and needs no terminal wrapped
    // round it; if it is not, a terminal running the monitor is the window,
    // and that is what this did before the GUI existed. Both attach to
    // whatever is serving the stream, so the choice is cosmetic and `--tui`
    // takes it back.
    if !tui {
        if let Some(g) = find_gui() {
            return spawn_window(Command::new(g)).map_or(Opened::Nothing, Opened::Window);
        }
    }
    let (term, flag) = match find_terminal() {
        Some(t) => t,
        None => {
            eprintln!("no terminal emulator found; run: radbeeper watch");
            return Opened::Nothing;
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
            return Opened::Nothing;
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
    spawn_window(cmd).map_or(Opened::Nothing, Opened::Window)
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
            child = match open_window(dir, device, baud, tui) {
                Opened::Window(c) => Some(c),
                Opened::Nothing => None,
                Opened::NotYet => {
                    w.not_yet(now(), SERVICE_WAIT);
                    None
                }
            };
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

    /// ONE TUBE WRITES NO MERGED FILE, and two do.
    ///
    /// With one counter there is nothing to merge and nothing to interleave,
    /// so the merged row would restate that counter's own log to more decimal
    /// places -- a second file to explain, to back up and to get out of step.
    /// The differential suite is what caught this: it asserts that a
    /// single-counter `watch` leaves exactly one `cpm-*.tsv` behind, which is
    /// the same rule stated from the outside.
    #[test]
    fn the_merge_begins_when_there_is_something_to_merge() {
        let spans = [3.0, 30.0];
        let dir = std::env::temp_dir()
            .join(format!("radbeeper-mergedlog-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = log::path(clock::now(), &dir, Some(log::MERGED));

        // One tube, for long enough that several intervals have gone by.
        let mut lg = MergedLogger::new(&spans, dir.clone(), 2.0);
        lg.counters(&["F48824B8207F7E".to_string()]);
        let mut all = Windows::new(&spans);
        for i in 0..20 {
            let when = 1_700_000_000.0 + i as f64;
            all.add(when, 2);
            assert_eq!(lg.add(0, when, 2, &all).unwrap(), None);
        }
        lg.finish(&all);
        assert!(!path.exists(), "one counter wrote a merged log");

        // A second joins. From here there IS a room to write down.
        lg.counters(&["F48824B8207F7E".to_string(), "F7F4CA7F05C2EA".to_string()]);
        let mut wrote = None;
        for i in 20..40 {
            let when = 1_700_000_000.0 + i as f64;
            for who in 0..2 {
                all.add(when + who as f64 * 0.5, 2);
                if let Some(line) = lg.add(who, when + who as f64 * 0.5, 2, &all).unwrap() {
                    wrote = Some(line);
                }
            }
        }
        let line = wrote.expect("two counters wrote a merged row");
        assert!(path.exists(), "and it went to the merged log");

        let names = log::columns(&log::merged_header(&spans));
        let cells: Vec<&str> = line.split('\t').collect();
        let at = |n: &str| cells[names.iter().position(|c| c == n).unwrap()];
        assert_eq!(at("tubes"), "2");
        // Both tubes in the breakdown, and an interleave now that there are
        // two clocks to measure between.
        assert!(at("per_tube").contains("F48824B8207F7E="), "{}", at("per_tube"));
        assert!(at("per_tube").contains("F7F4CA7F05C2EA="), "{}", at("per_tube"));
        assert!(!at("interleave").is_empty(), "no interleave was measured");
        let _ = std::fs::remove_dir_all(&dir);
    }

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

    /// THE LOGIN THAT OPENED NOTHING, pinned: the service held both ports for
    /// a sixteen-minute backfill, every try came back busy, and the event was
    /// written off before the socket appeared. Busy-and-not-serving spends no
    /// tries, however long it lasts, and the window opens once it is over.
    #[test]
    fn a_port_held_while_the_service_starts_up_spends_no_tries() {
        let mut w = Watcher::new(3, 2.0, true, 0.0);
        let mut t = 0.0;
        while t < 1000.0 {
            if w.should_open(t, false) {
                w.not_yet(t, 10.0);
            }
            t += 1.0;
        }
        // The backfill is over: the next look is still a try, and so are the
        // two after it if the window fails to open.
        let mut opens = 0;
        while t < 1100.0 {
            if w.should_open(t, false) {
                opens += 1;
            }
            t += 1.0;
        }
        assert_eq!(opens, 3, "all three tries were still there");
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
        assert_eq!(worth_a_window(&dir, Some("/dev/null-no-counter-here"), None), Worth::No);
        // A server on the socket, and the same impossible device: still yes.
        std::os::unix::net::UnixListener::bind(&sock).unwrap();
        assert_eq!(worth_a_window(&dir, Some("/dev/null-no-counter-here"), None), Worth::Yes);
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
