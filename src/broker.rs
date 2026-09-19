// SPDX-License-Identifier: MIT
// Copyright (c) 2026 Paul Richeson
// radbeeper -- the fan-out. One process reads the tube; everybody else reads
// this.
//
// WHY THIS EXISTS, AND WHY IT IS NOT A HANDOVER. serial.rs takes an exclusive
// flock for a reason that is exactly right and exactly narrow: two processes
// reading one tty each get a SHARE of the bytes and neither is told, so a
// logger and a monitor running together quietly halve both their counts. That
// argues for one READER. It does not argue for one CONSUMER, and for years it
// was read as though it did -- the service held the port, `watch` printed
// "port busy", and the only way to see your own counter was to stop the thing
// recording it.
//
// The counter sends two bytes a second. Everything this program draws -- five
// averaging windows, the cascade strip, the spectrum ladder, the entropy pool,
// the log rows, index.html -- is arithmetic over a stream of (when, counts).
// Nothing downstream of next_sample() touches the port. So the process holding
// the flock publishes what it reads, and anybody who wants the counter gets
// the samples instead of the device.
//
// WHAT A CLIENT NEVER DOES: open the port, write the log, write an emission.
// Those belong to whoever holds the flock, and the rule has no exceptions --
// it is what makes two windows and a service safe to run at once.
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Bumped when a line's meaning changes, never when one is added. A client
/// ignores lines it does not know, so a newer server can say more to an older
/// monitor without either of them having to care.
pub const PROTOCOL: u32 = 2;

/// How many samples a client is given on connect.
///
/// THE POINT OF THE WHOLE DESIGN IS IN THIS NUMBER. A monitor that attaches to
/// a running service does not start at zero: it is handed the history first,
/// so its 3-second window is full immediately and its 30000-second one -- the
/// working day, which takes eight and a half hours to earn -- is full if the
/// service has been up that long. Today's `watch` starts from nothing every
/// single launch and spends the first eight hours with a countdown where the
/// day average should be. 30000 samples is that longest default span, at 16
/// bytes each: half a megabyte to make a window you just opened complete.
const RING: usize = 30_000;

/// Log rows kept for a joining client's table, matching the monitor's own.
const ROWS: usize = 64;

/// Long enough for a client that is reading to take half a megabyte, short
/// enough that one that is not cannot hold up the counter. See publish().
const REPLAY_TIMEOUT: Duration = Duration::from_secs(5);

/// The socket, beside the log it belongs to.
///
/// NOT /run: the log directory is the one place both halves already agree on,
/// Copal creates it 2775 root:dialout, and a socket inheriting that group is
/// exactly the permission we want -- the same `dialout` membership that lets
/// you read the port lets you read the stream. A path under /run would need
/// its own directory, its own tmpfiles rule and its own reason.
pub fn socket_path(dir: &Path) -> PathBuf {
    dir.join("sock")
}

/// One counter on the other end of the port, as the server describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct CounterId {
    pub path: String,
    pub baud: u32,
    pub version: String,
    pub serial_no: String,
}

/// What the server is reading, and how it is cutting its log.
///
/// A LIST, BECAUSE TWO TUBES ARE TWO MEASUREMENTS OF ONE NUMBER. Each counter
/// keeps its own log and its own identity -- they are separate instruments and
/// the record has to say which said what -- while the display is free to
/// average them, which is the entire reason for running two.
#[derive(Clone, Debug, PartialEq)]
pub struct Identity {
    pub counters: Vec<CounterId>,
    /// The SERVER's averaging windows, which are the columns its log rows
    /// carry. A client draws its own spans from the samples -- that is local
    /// arithmetic and needs nobody's permission -- but the table at the
    /// bottom is the server's file, so it is drawn with the server's columns.
    pub spans: Vec<f64>,
}

impl Identity {
    /// The one to name when there is only room to name one.
    pub fn primary(&self) -> Option<&CounterId> {
        self.counters.first()
    }

    pub fn len(&self) -> usize {
        self.counters.len()
    }

    pub fn is_empty(&self) -> bool {
        self.counters.is_empty()
    }

    /// Every counter's port, for a `-d` that has to be matched against them.
    pub fn serves(&self, device: &str) -> bool {
        self.counters.iter().any(|c| c.path == device)
    }
}

/// One thing that happened, in the order it happened.
#[derive(Clone, Debug, PartialEq)]
pub enum Event {
    /// A second of one counter. `who` indexes Identity::counters. `when` is
    /// unix time, not boot-relative: a replayed sample has to mean something
    /// to a process that was not running when it was taken.
    Sample { who: usize, when: f64, counts: u32 },
    /// The server wrote this row to that counter's log. The client displays
    /// it and does not write it anywhere.
    Row { who: usize, row: String },
    /// That counter's pool drew a line. The hex on a client's screen is
    /// always this one, never a locally computed one -- see Feed in main.rs.
    Random { who: usize, hex: String, at: String, suspect: bool },
    /// A counter joined the running service. `who` is its index from here
    /// on, and it may be one already seen -- a tube that was unplugged and
    /// put back keeps the index its serial had.
    Counter { who: usize, id: CounterId },
    /// A counter stopped answering. Its index stays reserved for it.
    Gone { who: usize },
    /// Everything before this was history; everything after it is live.
    Live,
}

fn tab(fields: &[&str]) -> String {
    let mut s = fields.join("\t");
    s.push('\n');
    s
}

/// Serialise a time the way the log does: enough digits to keep the
/// millisecond, no more.
fn t(v: f64) -> String {
    format!("{:.3}", v)
}

/// The greeting for an identity: what is true now, in the order it is read.
fn greeting_for(id: &Identity) -> String {
    let spans = id.spans.iter().map(|s| crate::log::g(*s)).collect::<Vec<_>>().join(",");
    let mut out = tab(&[
        "hello",
        &PROTOCOL.to_string(),
        &spans,
        &id.counters.len().to_string(),
    ]);
    for (i, c) in id.counters.iter().enumerate() {
        out.push_str(&tab(&[
            "c",
            &i.to_string(),
            &c.path,
            &c.baud.to_string(),
            &c.version,
            &c.serial_no,
        ]));
    }
    out
}

// ------------------------------------------------------------------ server ---

/// The port-holder's end. Anything that owns the flock can run one.
pub struct Server {
    listener: UnixListener,
    path: PathBuf,
    clients: Vec<UnixStream>,
    ring: VecDeque<(usize, f64, u32)>,
    rows: VecDeque<(usize, String)>,
    id: Identity,
    greeting: String,
}

impl Server {
    /// Bind the socket, or say why not.
    ///
    /// A server that cannot bind is NOT a fatal condition for its caller: the
    /// service still logs and the monitor still draws, they just cannot be
    /// attached to. Returning the error and letting the caller carry on with
    /// a note is the whole handling this deserves.
    pub fn start(dir: &Path, id: &Identity) -> std::io::Result<Server> {
        let path = socket_path(dir);
        // A SOCKET OUTLIVES THE PROCESS THAT MADE IT. A service killed with
        // SIGKILL, or a machine that lost power, leaves the node in the
        // filesystem with nothing behind it; bind() then fails EADDRINUSE
        // forever and the fan-out never comes back. Connecting to it is the
        // only way to tell a live server from a dead one's leftovers.
        if path.exists() && UnixStream::connect(&path).is_err() {
            let _ = std::fs::remove_file(&path);
        }
        let listener = UnixListener::bind(&path)?;
        listener.set_nonblocking(true)?;
        // Group-readable, because the group is `dialout` and that is already
        // the answer to "may this person have the counter".
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o660));
        let greeting = greeting_for(id);
        Ok(Server {
            listener,
            path,
            clients: Vec::new(),
            ring: VecDeque::with_capacity(RING),
            rows: VecDeque::with_capacity(ROWS),
            id: id.clone(),
            greeting,
        })
    }

    pub fn socket(&self) -> &Path {
        &self.path
    }

    /// Rows already on disk, for the table a joining client draws.
    ///
    /// Without this, a monitor attaching to a service that started a minute
    /// ago gets an empty table and fills it one row every thirty seconds --
    /// while the log it is looking at has a month in it. The server replays
    /// what it has written since it started; this is what was there before.
    pub fn seed_rows<I: Iterator<Item = (usize, String)>>(&mut self, rows: I) {
        for r in rows {
            if self.rows.len() == ROWS {
                self.rows.pop_front();
            }
            self.rows.push_back(r);
        }
    }

    pub fn clients(&self) -> usize {
        self.clients.len()
    }

    /// Take whoever turned up since the last call, and hand them the history.
    ///
    /// Called once a second from the sample loop rather than from a thread of
    /// its own: a second is a perfectly good time to wait to be let in, and a
    /// thread here would need a lock around the ring that the counter path
    /// would then have to take sixty times a minute for no benefit at all.
    pub fn accept_pending(&mut self) {
        loop {
            match self.listener.accept() {
                Ok((stream, _)) => {
                    if let Some(c) = self.greet(stream) {
                        self.clients.push(c);
                    }
                }
                Err(_) => return,
            }
        }
    }

    /// The greeting, the history, and the line that says history is over.
    ///
    /// Written BLOCKING, with a deadline. Half a megabyte down a unix socket
    /// to a client that is reading takes microseconds; to one that is not it
    /// takes REPLAY_TIMEOUT and then the client is dropped. Doing this
    /// non-blocking instead would mean a partial replay and a monitor with a
    /// hole in its history, which is worse than no monitor.
    fn greet(&self, stream: UnixStream) -> Option<UnixStream> {
        stream.set_nonblocking(false).ok()?;
        stream.set_write_timeout(Some(REPLAY_TIMEOUT)).ok()?;
        let mut w = &stream;
        let mut out = String::with_capacity(self.ring.len() * 24 + 4096);
        out.push_str(&self.greeting);
        for (who, when, counts) in &self.ring {
            out.push_str(&tab(&["s", &who.to_string(), &t(*when), &counts.to_string()]));
        }
        for (who, r) in &self.rows {
            out.push_str(&tab(&["row", &who.to_string(), r]));
        }
        out.push_str("live\n");
        w.write_all(out.as_bytes()).ok()?;
        w.flush().ok()?;
        // LIVE TRAFFIC IS NON-BLOCKING AND THAT IS NOT NEGOTIABLE. From here
        // on a client that stops reading must cost the counter nothing: the
        // write fails with WouldBlock, the client is dropped, and the tube
        // goes on being read. A blocking write here would let one wedged
        // window stop the logger for every other process on the machine.
        stream.set_nonblocking(true).ok()?;
        Some(stream)
    }

    /// A counter has joined, or rejoined. Everyone attached is told, and
    /// everyone who attaches later is greeted with it.
    ///
    /// THE GREETING IS REBUILT, NOT APPENDED TO. A client that connects a
    /// minute after a tube was plugged in must be told about it in the same
    /// breath as the others -- there is no "since when" in this protocol and
    /// there does not need to be, because the greeting is simply what is true
    /// now.
    pub fn announce(&mut self, who: usize, id: &CounterId) {
        if who < self.id.counters.len() {
            self.id.counters[who] = id.clone();
        } else {
            self.id.counters.resize(who + 1, id.clone());
        }
        self.greeting = greeting_for(&self.id);
        self.publish(&tab(&[
            "c",
            &who.to_string(),
            &id.path,
            &id.baud.to_string(),
            &id.version,
            &id.serial_no,
        ]));
    }

    /// A counter stopped answering.
    pub fn publish_gone(&mut self, who: usize) {
        self.publish(&tab(&["gone", &who.to_string()]));
    }

    /// One second of one counter, to everyone attached.
    pub fn publish_sample(&mut self, who: usize, when: f64, counts: u32) {
        if self.ring.len() == RING {
            self.ring.pop_front();
        }
        self.ring.push_back((who, when, counts));
        self.publish(&tab(&["s", &who.to_string(), &t(when), &counts.to_string()]));
    }

    /// A row that has just gone into a counter's log.
    pub fn publish_row(&mut self, who: usize, row: &str) {
        if self.rows.len() == ROWS {
            self.rows.pop_front();
        }
        self.rows.push_back((who, row.to_string()));
        self.publish(&tab(&["row", &who.to_string(), row]));
    }

    /// A line a pool has earned. Clients display it; only the server writes it
    /// to the emission log, which is what keeps one hex line in one file.
    pub fn publish_random(&mut self, who: usize, hex: &str, at: &str, suspect: bool) {
        self.publish(&tab(&["r", &who.to_string(), hex, at, if suspect { "1" } else { "0" }]));
    }

    fn publish(&mut self, line: &str) {
        let bytes = line.as_bytes();
        self.clients.retain_mut(|c| match c.write_all(bytes) {
            Ok(()) => true,
            // A window that was closed, and a window that has stopped
            // reading, are the same thing from here and get the same
            // treatment. Neither is an error worth printing: people close
            // windows.
            Err(_) => false,
        });
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // So the next start does not have to guess whether the node is stale.
        let _ = std::fs::remove_file(&self.path);
    }
}

// ------------------------------------------------------------------ client ---

/// The attached end: a monitor, a probe, or a GUI that has not been written
/// yet. It never opens the port and never writes anything.
/// What one wake-up of `Client::poll` found.
///
/// THREE ANSWERS, NOT TWO. "nothing yet" and "there is no more counter" are
/// different facts about the world and a caller that cannot tell them apart
/// cannot poll at all.
#[derive(Debug, Clone, PartialEq)]
pub enum Poll {
    Event(Event),
    /// The timeout ran out with nothing whole on the wire. The server is
    /// still there.
    Idle,
    /// The socket ended. There is no more counter on it.
    Closed,
}

/// A read that ran out of time rather than out of socket.
///
/// BOTH KINDS, because which one a timed-out socket read returns is not the
/// same on every platform -- Linux gives `WouldBlock` for a receive timeout
/// and `TimedOut` for a connect one, and a client that only knew the first
/// would treat a quiet moment as a dead server.
fn would_block(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    )
}

pub struct Client {
    reader: BufReader<UnixStream>,
    pub identity: Identity,
    /// Bytes of a line that has not arrived whole yet.
    ///
    /// ONLY `poll` NEEDS THIS, AND IT IS WHY `poll` IS NOT `next` WITH A
    /// SHORTER ARGUMENT. `read_line` that times out mid-line has already
    /// taken those bytes out of the socket and hands back an error rather
    /// than the fragment, so waking every tenth of a second would eat a
    /// sample whenever a wake-up landed between the `s` and the newline.
    /// Held here, the fragment is simply the start of the next read.
    partial: Vec<u8>,
}

impl Client {
    /// Attach, or None if nothing is serving.
    ///
    /// None is the ordinary case, not a failure: it means no service is
    /// running and the caller should open the port itself.
    pub fn attach(dir: &Path) -> Option<Client> {
        Client::connect(&socket_path(dir))
    }

    pub fn connect(path: &Path) -> Option<Client> {
        let stream = UnixStream::connect(path).ok()?;
        // The greeting is the first thing on the wire and arrives at once. A
        // server that has bound the socket but cannot speak is indisposed,
        // and waiting on it forever would hang the monitor at startup.
        stream.set_read_timeout(Some(Duration::from_secs(5))).ok()?;
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let f: Vec<&str> = line.trim_end().split('\t').collect();
        if f.len() < 4 || f[0] != "hello" || f[1].parse::<u32>().ok()? != PROTOCOL {
            return None;
        }
        let spans: Vec<f64> = f[2].split(',').filter_map(|s| s.parse().ok()).collect();
        let n: usize = f[3].parse().ok()?;
        let mut counters = Vec::with_capacity(n);
        for _ in 0..n {
            let mut l = String::new();
            reader.read_line(&mut l).ok()?;
            let g: Vec<&str> = l.trim_end().split('\t').collect();
            if g.len() < 6 || g[0] != "c" {
                return None;
            }
            counters.push(CounterId {
                path: g[2].to_string(),
                baud: g[3].parse().ok()?,
                version: g[4].to_string(),
                serial_no: g[5].to_string(),
            });
        }
        let identity = Identity { counters, spans };
        Some(Client { reader, identity, partial: Vec::new() })
    }

    /// The next thing that happened, or None if the feed ended.
    ///
    /// None here means what None from next_sample() means: there is no more
    /// counter. The server exiting, the socket closing and a server that has
    /// gone quiet are all the same answer to the caller, which is to stop.
    pub fn next(&mut self, timeout: Duration) -> Option<Event> {
        let _ = self.reader.get_ref().set_read_timeout(Some(timeout));
        loop {
            let mut line = String::new();
            match self.reader.read_line(&mut line) {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            if let Some(e) = parse(line.trim_end()) {
                // THE IDENTITY IS KEPT UP TO DATE HERE, so a caller that asks
                // `identity()` after a tube joined gets the tube. Every client
                // would otherwise have to do this itself, and one that forgot
                // would index a sample into a counter list too short for it.
                self.absorb(&e);
                return Some(e);
            }
            // An unknown verb is skipped, not fatal: see PROTOCOL.
        }
    }

    /// The next thing that happened, a quiet moment, or the end of the feed.
    ///
    /// WHY THIS IS NOT `next` WITH A SMALLER TIMEOUT. `next` collapses a
    /// timeout and a closed socket into the same `None`, which is right when
    /// the timeout is ten seconds and a silence that long means the server
    /// has gone. It is wrong when the caller wants to wake up ten times a
    /// second to move a needle: every wake-up would read as a dead counter.
    /// So this keeps them apart, and keeps the half-line a short timeout can
    /// leave behind (see `partial`).
    pub fn poll(&mut self, timeout: Duration) -> Poll {
        let _ = self.reader.get_ref().set_read_timeout(Some(timeout));
        loop {
            let mut chunk = Vec::new();
            let read = self.reader.read_until(b'\n', &mut chunk);
            // THE BYTES ARE KEPT BEFORE THE RESULT IS LOOKED AT. `read_until`
            // appends as it goes and can append and THEN fail -- a timeout
            // landing between the `s` and the newline returns an error with
            // the first half of the line already taken out of the socket. Read
            // the result first and those bytes are gone, and the sample with
            // them.
            self.partial.extend_from_slice(&chunk);
            match read {
                // `read_until` only returns Ok when it found the newline or
                // ran out of socket, so an Ok with nothing at the end of it is
                // the end of the feed either way.
                Ok(_) if self.partial.last() == Some(&b'\n') => {}
                Ok(_) => return Poll::Closed,
                Err(e) if would_block(&e) => return Poll::Idle,
                Err(_) => return Poll::Closed,
            }
            let line = String::from_utf8_lossy(&self.partial).trim_end().to_string();
            self.partial.clear();
            if let Some(e) = parse(&line) {
                self.absorb(&e);
                return Poll::Event(e);
            }
            // An unknown verb is skipped, not fatal: see PROTOCOL.
        }
    }

    /// A counter line changes who `who` means, so it is applied before the
    /// event is handed on -- however the event was read.
    fn absorb(&mut self, e: &Event) {
        if let Event::Counter { who, id } = e {
            if *who < self.identity.counters.len() {
                self.identity.counters[*who] = id.clone();
            } else {
                self.identity.counters.resize(*who + 1, id.clone());
            }
        }
    }

    /// Everything the greeting already told us, for a `probe` that cannot
    /// have the port. It is most of what probe prints, and it comes without
    /// asking the counter anything -- which matters, because the counter is
    /// mid-stream and interrupting it costs a sample.
    pub fn identity(&self) -> &Identity {
        &self.identity
    }
}

fn parse(line: &str) -> Option<Event> {
    let f: Vec<&str> = line.split('\t').collect();
    match *f.first()? {
        "s" if f.len() >= 4 => Some(Event::Sample {
            who: f[1].parse().ok()?,
            when: f[2].parse().ok()?,
            counts: f[3].parse().ok()?,
        }),
        "row" if f.len() >= 3 => Some(Event::Row {
            who: f[1].parse().ok()?,
            row: f[2..].join("\t"),
        }),
        "r" if f.len() >= 5 => Some(Event::Random {
            who: f[1].parse().ok()?,
            hex: f[2].to_string(),
            at: f[3].to_string(),
            suspect: f[4] == "1",
        }),
        "c" if f.len() >= 6 => Some(Event::Counter {
            who: f[1].parse().ok()?,
            id: CounterId {
                path: f[2].to_string(),
                baud: f[3].parse().ok()?,
                version: f[4].to_string(),
                serial_no: f[5].to_string(),
            },
        }),
        "gone" if f.len() >= 2 => Some(Event::Gone { who: f[1].parse().ok()? }),
        "live" => Some(Event::Live),
        _ => None,
    }
}

// ------------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> Identity {
        Identity {
            counters: vec![CounterId {
                path: "/dev/ttyUSB0".into(),
                baud: 115200,
                version: "GMC-320Re 4.26".into(),
                serial_no: "F48824B8207F7E".into(),
            }],
            spans: vec![3.0, 30.0, 300.0, 3000.0, 30000.0],
        }
    }

    /// Two counters, as a machine with a pair plugged in describes itself.
    fn pair() -> Identity {
        let mut id = id();
        id.counters.push(CounterId {
            path: "/dev/ttyUSB1".into(),
            baud: 115200,
            version: "GMC-320Re 4.26".into(),
            serial_no: "AA1122BB3344CC".into(),
        });
        id
    }

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir()
            .join(format!("radbeeper-test-{}-{}", name, std::process::id()));
        let _ = std::fs::create_dir_all(&d);
        let _ = std::fs::remove_file(socket_path(&d));
        d
    }

    /// Attach a client to a server that is not in its sample loop yet.
    ///
    /// CONNECT HAPPENS BEFORE ACCEPT, ALWAYS. The greeting is written when
    /// the server accepts, so a client cannot finish attaching until the
    /// server's loop comes round -- which in the real program is at most one
    /// second, and in a test is this. The first version of these tests called
    /// attach() and accept_pending() in that order on one thread and deadlocked
    /// for five seconds before every assertion.
    fn attached(dir: &Path, s: &mut Server) -> Client {
        let d = dir.to_path_buf();
        let want = s.clients() + 1;
        let joining = std::thread::spawn(move || Client::attach(&d));
        for _ in 0..500 {
            s.accept_pending();
            if s.clients() >= want {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        joining.join().unwrap().expect("attached to the server")
    }

    /// A QUIET MOMENT IS NOT A DEAD SERVER. `next` cannot say which is which
    /// and does not need to at ten seconds; a window waking ten times a second
    /// to move a needle would read every single wake-up as the counter having
    /// stopped.
    #[test]
    fn polling_tells_a_silence_apart_from_a_closed_socket() {
        let dir = tmp("poll-idle");
        let mut s = Server::start(&dir, &id()).expect("served");
        let mut c = attached(&dir, &mut s);

        // Past the greeting's tail -- the `live` marker that ends the
        // backfill is a real event and arrives before any silence does.
        let mut idle = 0;
        for _ in 0..50 {
            if c.poll(Duration::from_millis(20)) == Poll::Idle {
                idle += 1;
            }
        }
        // Nothing is being published, so nearly every one of those was a
        // quiet moment -- and not one of them was the end of the feed.
        assert!(idle >= 40, "only {} of 50 wake-ups were quiet", idle);

        // A sample still arrives through the same call.
        s.publish_sample(0, 1_700_000_000.5, 12);
        let mut got = None;
        for _ in 0..50 {
            match c.poll(Duration::from_millis(20)) {
                Poll::Event(Event::Sample { who, when, counts }) => {
                    got = Some((who, when, counts));
                    break;
                }
                Poll::Idle => continue,
                other => panic!("{:?}", other),
            }
        }
        assert_eq!(got, Some((0, 1_700_000_000.5, 12)));

        // And when the server really does go, that is a different answer.
        drop(s);
        let mut closed = false;
        for _ in 0..50 {
            if c.poll(Duration::from_millis(20)) == Poll::Closed {
                closed = true;
                break;
            }
        }
        assert!(closed, "a dropped server ends the feed");
    }

    /// A WAKE-UP MID-LINE MUST NOT EAT THE LINE. `read_until` appends what it
    /// got and then reports the timeout, so the half-line has already left the
    /// socket: dropped, the sample goes with it. Polled fast against a server
    /// publishing slowly, every sample still arrives exactly once.
    #[test]
    fn a_timeout_landing_mid_line_keeps_the_line() {
        let dir = tmp("poll-partial");
        let mut s = Server::start(&dir, &id()).expect("served");
        let mut c = attached(&dir, &mut s);
        let mut seen: Vec<u32> = Vec::new();
        for i in 0..40u32 {
            s.publish_sample(0, 1_700_000_000.0 + i as f64, i);
            // Far shorter than the publishing beat, which is the case that
            // lands a timeout in the middle of a line.
            for _ in 0..3 {
                if let Poll::Event(Event::Sample { counts, .. }) =
                    c.poll(Duration::from_millis(1))
                {
                    seen.push(counts);
                }
            }
        }
        for _ in 0..200 {
            match c.poll(Duration::from_millis(5)) {
                Poll::Event(Event::Sample { counts, .. }) => seen.push(counts),
                _ => break,
            }
        }
        assert_eq!(seen, (0..40u32).collect::<Vec<u32>>(),
                   "every sample, once, in order");
    }

    /// The identity survives the wire, spaces in the firmware string and all.
    /// A version of "GMC-320Re 4.26" split on whitespace is why the protocol
    /// is tabs and not spaces.
    #[test]
    fn a_client_learns_what_the_counter_is_before_any_sample_arrives() {
        let dir = tmp("identity");
        let mut s = Server::start(&dir, &id()).unwrap();
        let c = attached(&dir, &mut s);
        assert_eq!(c.identity(), &id());
        assert_eq!(c.identity.primary().unwrap().version, "GMC-320Re 4.26");
    }

    /// THE POINT OF THE DESIGN, as a test: a monitor that turns up late is
    /// given every sample the server has, so its windows are full rather than
    /// filling. Without this a GUI you close and reopen is worthless.
    #[test]
    fn attaching_late_replays_the_history_before_the_live_line() {
        let dir = tmp("replay");
        let mut s = Server::start(&dir, &id()).unwrap();
        for i in 0..100 {
            s.publish_sample(0, 1_000_000.0 + i as f64, i % 7);
        }
        let mut c = attached(&dir, &mut s);
        let mut got = Vec::new();
        loop {
            match c.next(Duration::from_secs(2)) {
                Some(Event::Sample { when, counts, .. }) => got.push((when, counts)),
                Some(Event::Live) => break,
                Some(_) => {}
                None => panic!("the feed ended before the history did"),
            }
        }
        assert_eq!(got.len(), 100, "every sample the server held");
        assert_eq!(got[0], (1_000_000.0, 0));
        assert_eq!(got[99], (1_000_099.0, 99 % 7));
    }

    /// The ring is a ring: an old sample is dropped, not kept forever.
    #[test]
    fn the_history_stops_at_the_longest_window_it_has_to_fill() {
        let dir = tmp("ring");
        let mut s = Server::start(&dir, &id()).unwrap();
        for i in 0..(RING + 500) {
            s.publish_sample(0, i as f64, 1);
        }
        assert_eq!(s.ring.len(), RING);
        assert_eq!(s.ring.front().unwrap().1, 500.0, "the oldest 500 fell off");
    }

    /// Live samples reach an attached client, and a row and an emission are
    /// carried as themselves rather than as something the client must guess.
    #[test]
    fn what_the_server_publishes_is_what_the_client_receives() {
        let dir = tmp("live");
        let mut s = Server::start(&dir, &id()).unwrap();
        let mut c = attached(&dir, &mut s);
        assert_eq!(c.next(Duration::from_secs(2)), Some(Event::Live));
        s.publish_sample(0, 1_700_000_000.5, 12);
        s.publish_row(0, "2026-09-18 11:00:00\t0.4\t12\t30.0");
        s.publish_random(0, "abcd", "11:00:30", true);
        assert_eq!(
            c.next(Duration::from_secs(2)),
            Some(Event::Sample { who: 0, when: 1_700_000_000.5, counts: 12 })
        );
        assert_eq!(
            c.next(Duration::from_secs(2)),
            Some(Event::Row { who: 0, row: "2026-09-18 11:00:00\t0.4\t12\t30.0".into() })
        );
        assert_eq!(
            c.next(Duration::from_secs(2)),
            Some(Event::Random {
                who: 0, hex: "abcd".into(), at: "11:00:30".into(), suspect: true
            })
        );
    }

    /// A window that was closed costs the counter nothing: the write fails,
    /// the client is forgotten, and the next sample is published as usual.
    #[test]
    fn a_client_that_went_away_is_dropped_and_the_rest_carry_on() {
        let dir = tmp("gone");
        let mut s = Server::start(&dir, &id()).unwrap();
        let going = attached(&dir, &mut s);
        let mut staying = attached(&dir, &mut s);
        assert_eq!(s.clients(), 2);
        drop(going);
        // Two, because a closed socket is not noticed until a write to it
        // fails, and the first write is the one that fails.
        for i in 0..2 {
            s.publish_sample(0, i as f64, 3);
        }
        assert_eq!(s.clients(), 1, "the closed one was dropped");
        assert_eq!(staying.next(Duration::from_secs(2)), Some(Event::Live));
        assert_eq!(
            staying.next(Duration::from_secs(2)),
            Some(Event::Sample { who: 0, when: 0.0, counts: 3 })
        );
    }

    /// A socket left behind by a killed service is not a permanent outage.
    ///
    /// A SIGKILL runs no Drop, so the node stays in the filesystem with
    /// nothing behind it. Dropping a bare listener without unlinking it is
    /// that state exactly: the file is there, and connecting to it fails.
    #[test]
    fn a_stale_socket_from_a_killed_server_is_replaced_not_obeyed() {
        let dir = tmp("stale");
        let path = socket_path(&dir);
        drop(UnixListener::bind(&path).unwrap());
        assert!(path.exists(), "the node outlived the server, as after a kill");
        assert!(UnixStream::connect(&path).is_err(), "and nothing is behind it");
        let s = Server::start(&dir, &id()).expect("bound over the stale node");
        assert!(s.socket().exists());
    }

    /// A LIVE server is not stepped on, whatever the first one thinks.
    #[test]
    fn a_second_server_does_not_take_a_socket_that_is_answering() {
        let dir = tmp("taken");
        let first = Server::start(&dir, &id()).unwrap();
        assert!(Server::start(&dir, &id()).is_err(), "the live one keeps it");
        assert!(first.socket().exists());
    }

    /// TWO TUBES, ONE STREAM. Both counters' identities reach a client, and
    /// every sample says which of them it came from -- without that the
    /// averaging on the other end would be adding one counter's counts to the
    /// other's and calling the result twice the dose.
    #[test]
    fn a_pair_of_counters_arrives_as_a_pair_and_stays_told_apart() {
        let dir = tmp("pair");
        let mut s = Server::start(&dir, &pair()).unwrap();
        let mut c = attached(&dir, &mut s);
        assert_eq!(c.identity(), &pair());
        assert_eq!(c.identity().len(), 2);
        assert_eq!(c.identity().counters[1].serial_no, "AA1122BB3344CC");
        assert!(c.identity().serves("/dev/ttyUSB1"));
        assert_eq!(c.next(Duration::from_secs(2)), Some(Event::Live));
        s.publish_sample(1, 10.0, 7);
        s.publish_sample(0, 10.5, 3);
        assert_eq!(
            c.next(Duration::from_secs(2)),
            Some(Event::Sample { who: 1, when: 10.0, counts: 7 })
        );
        assert_eq!(
            c.next(Duration::from_secs(2)),
            Some(Event::Sample { who: 0, when: 10.5, counts: 3 })
        );
    }

    /// Nothing serving is not an error. It is how the monitor knows to open
    /// the port itself.
    #[test]
    fn attaching_to_nothing_is_a_plain_no() {
        let dir = tmp("empty");
        assert!(Client::attach(&dir).is_none());
    }
}
