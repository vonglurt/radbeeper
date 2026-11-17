# A user experience review

**What it is like to install and use RadBeeper, from the first command to the
daily glance: what already works, forty findings, nearly all with a fix, and the
ten changes that would help a user most.**

*16 November 2026, against commit `cf8eb39` with the working tree's
uncommitted changes built in. Done on a Mac with a fake counter
(`tests/fake_gmc.py`) and 45 days of made-up logs (`examples/sample_logs.rs`):
every command was run and its output read, the terminal monitor was captured
at five sizes, and the window and the pages were rendered to pictures and
read, not clicked. No real counter was plugged in at any point.*

---

## The verdict

Once a counter is being read, RadBeeper is good to use. The terminal monitor
fits an 80 by 24 screen with nothing cut off, the window and the pages are
full the moment they open because they are made from the log, and almost
every waiting state says how long the wait is. The trouble is at the edges.
A person with no counter, no service, a mistyped command or a Mac is told
too little, or is told to run a Linux command. The window never says that it
has keys or that its parts can be clicked. None of this is deep: most of the
ten changes at the end are a line of text each.

---

## What works well

| | The evidence |
|---|---|
| **Waiting states say how long** | A fresh terminal monitor prints `30s   filling, 21s to go`, `spectrum   accumulating, 120s to the first window of 128` and, where a day paper exists, `now   the first row in 432s` (`tui-fresh-80x24.txt`, `tui-80x24.txt`). |
| **80 by 24 is a whole monitor** | Header, five averages, thermometer, a folded day line, the live drum, the counts and the key line all fit, with no wrapped or cut line (`tui-80x24.png`). At 120 by 40 and 160 by 50 the same screen gains the papers in full and the log table (`tui-120x40.png`, `tui-160x50.txt`). |
| **256 colours lose nothing visible** | With `COLORTERM` unset the monitor sends `38;5;` colours only, and the picture reads the same as the 24-bit one (`tui-120x40-256.png` beside `tui-120x40.png`). |
| **`now` is a good interface for a program** | With no service it exits 1 and says what to do. With one it prints a header line and a row, or one JSON object with an age beside every value. Six seconds after the fake counter was stopped the JSON said `"live":false`. |
| **The service says what it is doing** | Its start is twelve plain lines, one fact each: the port, the files, the row spacing, the socket, the first temperature (`service.out`). |
| **Exit codes are consistent** | 2 for an unknown option, 1 for no counter and for no service, 0 for a dormant service. |
| **The drums page explains itself** | Every chart on `drums.html` has a paragraph above saying how to read it and a caption below saying what it was made from, and the temperature section ends "the inside of the counter's case, not the air of the room" (`view-drums.png`). Neither generated page contains a script. |
| **The front page says what the thing is** | The first screen at 1200 by 900 has the sentence "RadBeeper reads a GQ GMC-320 Plus over USB", an Install button and the top of a picture of the window (`page-front.png`). |
| **The window keeps its shape** | At 760 by 900, 1262 by 756 and 560 by 400, in both themes, nothing overlaps and the dials, the big number and the counts stay readable (`gui-760x900-dark.png`, `gui-1262x756-antiquity.png`, `gui-560x400-dark.png`). The one exception is finding 4.3. |

---

## Findings

Severity is one of three: **blocks** (a user cannot go on without reading the
source or asking), **confuses** (a user goes on, with a wrong idea or after a
detour), **polish**. Pictures and captures are in `/tmp/rbux-review/`; they
are not in the repository.

### 1. First run and install

**1.1 The window needs a service, and the README's first run does not say so
(confuses).** The README lists `probe`, `watch`, `radbeeper-gui` ("or the same
counter in a window"), then `service`. `radbeeper-gui --logs /tmp/rbux-none`
with nothing serving drew two lines, "no counter" and "nothing is serving
/tmp/rbux-none/sock" (`gui-none-dark.png`). A Linux user off Copal who types
the third line of the README gets that screen with a counter plugged in and
working. *Fix:* put `radbeeper service` second in the list, and say in one
sentence that the window attaches to a running `service` or `watch`.

**1.2 The tree and the release are both 0.6.0 (confuses).** The README says
the temperature, the long drums and the Mac build are "in this repository,
not yet in a release", and the release is 0.6.0. `radbeeper --version` from
the tree prints `radbeeper 0.6.0`, and the window's corner reads "radbeeper
0.6.0". A user cannot tell from the program which of the two they are
running, and neither can a bug report. *Fix:* call the tree `0.7.0-dev`, or
print the commit after the version.

**1.3 The first eight and a half minutes of the window are a black
rectangle (confuses).** With a new log directory the window's largest panel,
about a third of its height, is black and carries no words
(`gui-fresh-dark.png`). The terminal monitor in the same state says
`spectrum   accumulating, 120s to the first window of 128`. This is the first
thing a new user sees after the dials. *Fix:* write the terminal's sentence
in the empty paper.

**1.4 On a Mac the app alone leaves no `radbeeper` in the shell (confuses).**
On this Mac `/Applications/RadBeeper.app` is installed and `which radbeeper
radbeeper-gui` finds neither; the program is inside the app as
`radbeeper-cli`. The README's fleet section and every document say
`radbeeper now`. `make install` would put it on the path; `make app` does
not say that it has not. *Fix:* have `make app` end with a line saying where
the program is and that `make install` puts it on the path.

### 2. The command line

**2.1 An unknown command is reported as a missing counter (confuses).**

```
$ radbeeper frobnicate
no counter: no serial device is present
    Nothing matching /dev/ttyUSB* or /dev/ttyACM*.
[exit 1]
```

`radbeeper help` gives the same. The check for the command's name is the last
arm of the match that runs after a counter has been found
(`src/main.rs`, "unknown command"), so only a user with a counter plugged in
is told the truth. An unknown option is caught at once: `radbeeper:
unknown option --frobnicate`, exit 2. *Fix:* check the name first; make
`help` print the help.

**2.2 With the service running, `cpm` says the port is busy, and the message
says that cannot happen (confuses).** `radbeeper -d /dev/ttys002 cpm`, with a
service of this build holding that port:

```
port busy: the port is already open by another radbeeper
    ...A `service` from this version hands the stream to anyone
    who asks and never produces this message, so the holder is a
    one-shot that wants the port to itself...
    doas rc-service radbeeper stop
```

The holder was a `service` from this version. `cpm` is the command the
README gives "for a script", and on Copal the service is always running, so
this is what that script gets. `clock` and `backfill` behave the same way;
`backfill` says only `no counter: the port is already open by another
radbeeper`, under the heading "no counter" and with no next step. *Fix:* let
`cpm` answer from the service when one is serving, as `probe` does; until
then, have the busy message name `radbeeper now`.

**2.3 A missing argument is not an error (polish).** `radbeeper --logs`
prints the whole help and exits 0. `radbeeper random --check` with no file
says `no counter: no serial device is present`; with a file that does not
exist it says `radbeeper: no emissions in /tmp/nonexistent.tsv`. *Fix:*
"--logs needs a directory", "--check needs a file", "no such file", each
with exit 2 or 1.

**2.4 `export` over a directory with no logs writes an empty page and exits 0
(confuses).** `radbeeper --logs /tmp/rbux-empty export` in a scratch
directory printed `radbeeper: index.html -- 0 counters, 0 rows` and wrote the
file. A mistyped `--logs` publishes a blank report. *Fix:* exit 1 with "no
cpm-*.tsv in DIR" and write nothing.

**2.5 The help is one flat list of 20 commands and 39 options (polish).**
Options are marked by command only some of the time (`hotplug:`, `export:`,
`service:`), `-o, --output` appears twice with two meanings, and the text
ends "site and recompute are in the Python program in the same repository.
They are being ported". The README's command table lists `site` with no
such note, and `radbeeper site` exits 2 pointing at the GitHub address
although the Python `radbeeper` is in the checkout's root. *Fix:* group the
options under the command that takes them; mark `site` in the README.

**2.6 `watch` into a pipe sends the full-screen escape codes (polish).**
`radbeeper watch | cat` begins `^[[?1049h^[[?25l^[[2J`. There is no plain
mode: `watch --plain` is `radbeeper: unknown option --plain`. `preview
--duration 1` ignored the duration and served until it was killed. *Fix:*
when standard output is not a terminal, print one line a row, or refuse and
name `now`.

**2.7 `now` in its first seconds labels 17 seconds as five minutes
(polish).** Seventeen seconds after the service started, `now` printed
`cpm 37.5` and `cpm_300 37.5`. The JSON carries `"cpm_300_seconds":17`; the
plain form has no such column. *Fix:* print `-` in the plain form until the
window is full, as the log table does.

### 3. The terminal monitor

**3.1 The key line states what a paper is, not what the key will do
(confuses).** The last line reads `q to quit   t temperature   d days
folded   m months off`. `t temperature` is an action; `d days folded` is a
state. `d` moves through three states (shown, folded, off) and nothing on
screen says there are three. The tour documents it; the screen does not.
*Fix:* `d days: folded → off`, or the three words with the present one
bright.

**3.2 The `d` and `m` hints can disappear with the papers (confuses).** At 80
by 24, a few seconds after opening, pressing `d` once took the day line off,
and the key line became `q to quit   t temperature` (`tui-80x24-d.png`).
The key that would bring the paper back was no longer listed. The hints are
drawn only when a paper or a live row is on screen
(`src/main.rs`, the block that builds `keys`), so this lasts until the live
drum has its first row. *Fix:* list `d` and `m` whenever the log has day or
month lines, whatever is showing.

**3.3 Several marks have no label (polish).** From `tui-120x40.png`: the
large digits at the top right are the 30-second reading and say so nowhere;
the day line ends `20m ×22`; the counts' ruler reads `F 64s/bar` and `F
1s/bar · 19s`; a line reads `random   measuring the source (4s)`. Each is
explained in a document. On the screen a new user has four things to look
up. *Fix:* `30s` under the digits; drop or spell out the `F`.

**3.4 "run" is not this run (polish).** Five seconds after `watch` opened
over the sample logs the screen read `run  19911 counts in 30081s`. The
figure comes from the log the monitor read at start. With a new log it read
`run  8 counts in 9s`. *Fix:* call it `log` or `window` when it was seeded.

**3.5 Below 80 columns the header wraps (polish).** At 60 by 15 the first
line wrapped into the second, the thermometer's column was past the edge of
the spectrum, and there was no key line (`tui-60x15.txt`). *Fix:* cut the
header to the width, as the key line already is.

**3.6 The temperature is in Fahrenheit first, with no way to change it
(polish).** The monitor, the window and the drums page put °F in bold and °C
dim; the scale is fixed at 50 to 100 °F. The log, `now` and the service's
own messages are in °C only. A reader outside the United States reads the
smaller number. *Fix:* an option, or follow the locale; say what happens
below 50 °F.

### 4. The window

All of this section is judged from rendered pictures. Nothing was clicked
and no key was pressed.

**4.1 Nothing on the window says it has keys or that anything can be clicked
(confuses).** In every picture taken, the tiles are plain boxes captioned
"day drum", "month drum" and "room temp", the thermometer is a drawing, and
no line names `d`, `m`, `t` or `q` (`gui-760x900-dark-tiles.png`). The only
places a user learns of them are `radbeeper-gui --help` and the documents.
The terminal monitor has a key line; the window, which has more room, does
not. *Fix:* one dim line at the foot, "d days · m months · t temperature · q
quit · click a tile to open it".

**4.2 "no counter" gives no next step (blocks).** The whole window is "no
counter / nothing is serving /Users/user/.local/share/radbeeper/sock"
(`app-no-counter.png`). A person who double-clicked an icon is shown a socket
path. *Fix:* a third line: on Linux "start it with: radbeeper service"; in
the Mac app "plug the counter in and switch it on", with what the service
last said (the `status` file has it).

**4.3 In a narrow window the thermometer is drawn over the reading (bug).**
At 420 by 1000 the thermometer and its two labels sit on top of "CPM ±8
(23.6%)", the dose rate and the right-hand column of the averages
(`gui-420x1000-dark-all.png`). *Fix:* drop the thermometer to a number, or
move it under the dials, when the dial band has no room.

**4.4 At 560 by 400 the papers and their tiles vanish and leave a gap
(confuses).** By default the band between the dials and the counts is empty:
no paper and no tile (`gui-560x400-dark.png`). With `--show
days,months,temp` only a three-label temperature strip appears, the days and
months are dropped without a word, and a blank band remains under it
(`gui-560x400-antiquity-all.png`). *Fix:* keep the tiles at this size, since
a tile is the small form.

**4.5 The live drum has no title, and is black until its first line
(confuses).** The day paper is headed "8h · days · a line a day" and the
month paper "10d · months". The live drum below them has no heading. With
all three papers folded it is a black rectangle of about a third of the window
between the dials and the counts (`gui-760x900-dark-tiles.png`). See also
1.3. *Fix:* head it "live · a line every 8 s".

**4.6 The date labels are small and the lines run through them (polish).**
On the day paper the dates sit under the pen: in `gui-760x900-dark.png` "01
Nov" and "13 Nov" are struck through by their own lines. At 1262 by 756 the
labels are about 8 pixels tall in a picture at roughly the window's own
size. Dates are also written four ways: "10 Nov" on the day paper, "11-07"
on the window's temperature paper, "Nov 07" on the terminal's, "7 Nov" on
the page. *Fix:* a margin for the labels as the temperature paper has; one
date form.

**4.7 Small wording (polish).** The tile says "room temp" where every
document says the inside of the counter's case. The temperature paper's
heading gives "3.00 °C (5.40 °F)", Celsius first, beside labels that are
Fahrenheit first. The second dial's caption, "1s 60 · 30s 40 / 3s held ·
1–99", needs its document to read.

**4.8 The antiquity theme's papers are harder to read (polish).** The papers
are brown with green lines and pale labels
(`gui-760x900-antiquity-all.png`). From the picture, the ordinary green line
and the date labels stand out less than on the dark theme's black. This is
a judgement from one rendering, not a measurement.

### 5. The pages

**5.1 The report does not link to the drums page (confuses).** `export
--drums-page` wrote both. `drums.html` opens with "← Radiation monitor";
`monitor.html` has no link to `drums.html`, in my export or in the scratch
build of the site, and no link to the front page. A visitor who arrives on
the report cannot reach the days and months. *Fix:* a link line at the top
of `monitor.html`, the same shape as the one on `drums.html`.

**5.2 The report describes the author's machine as its own (confuses).** A
page exported on a Mac from `/tmp/rbux` begins "A GQ GMC-320 Plus
Geiger–Müller counter, plugged into a machine running Alpine Linux ... here
shared into a virtual machine over USB pass-through ... this page is
regenerated from those files by a GitHub Action on every push"
(`view-monitor.png`). None of it was true of that page. *Fix:* a neutral
paragraph by default, and the present one behind an option the repository's
own build passes.

**5.3 "how to read the drum" is a dead link outside the repository
(confuses).** `drums.html` links to `docs/reading-the-drum-spectrogram.html`,
which exists only where the site is built. The log links in `monitor.html`
are relative to where the page was written (`../rbux/cpm-…`), so they break
when the page is moved. *Fix:* the published address for the first; copy or
omit for the second.

**5.4 The front page says Linux three times and Mac not once on its first
screen (polish).** The eyebrow is "GQ GMC-320 PLUS · USB · LINUX", the title
tag is "Geiger counter data logging for Linux", and a pill reads "Alpine &
Copal Linux" (`page-front.png`). The README says "Linux and macOS". Of the
eight buttons, "Architecture" is for a contributor, and in the scratch build
it was a dead link because that document was still being written; check it
after both new documents exist.

**5.5 "±0.0 CPM" (polish).** The first card on the report reads "PRECISION
±0.0 CPM, 0.06% of the mean". *Fix:* two significant figures.

### 6. The documents

**6.1 Troubleshooting and Running it do not mention the Mac (blocks a Mac
user).** Neither file contains "Mac" or "macOS". Troubleshooting speaks of
`/dev/ttyUSB*`, `dmesg`, `dialout`, `/var/lib/radbeeper/sock` and
`rc-service`. The README sends every reader there: "Troubleshooting has the
fix for each". *Fix:* a short Mac section in each: what the port is called,
that the log is in `~/.local/share/radbeeper`, how to see whether the
service is running and how to stop it.

**6.2 Running it opens with a migration (polish).** Its second section is
fourteen lines of `doas` and `sed` over `/etc/init.d/radbeeper` to move a log
directory. A newcomer cannot tell whether it applies to them. *Fix:* say at
its head who needs it, or move it to the archive.

**6.3 The README is the right length (no change).** It was read top to
bottom as a newcomer would. What it does, what you need, install, first run
and the commands fit in one sitting, and "Learn more" picks eleven documents
out of some thirty in `docs/`.

### 7. The Mac

**7.1 A counter plugged in after the app is opened is never seen (blocks).**
Running the app's launcher with no counter started a service, which wrote
`radbeeper: dormant` and exited 0; the window showed "no counter". Nothing
then looks again. `packaging/RadBeeper.sh` says in a comment "plug one in and
open the app again", which the user never sees, and a second double click on
an open app does not run the launcher again: the app has to be quit first.
Plugging in was not tested, there being no counter; this is from the script
and from the service's observed exit. *Fix:* have the launcher, or the
service under an option, wait for a counter the way `--wait` waits for a
port; and see 4.2.

**7.2 Messages on a Mac name Linux commands and Linux ports (confuses).**
Each of these was printed on this Mac:

```
no counter: no serial device is present
    Nothing matching /dev/ttyUSB* or /dev/ttyACM*.
```
```
    it will look again at the next boot, or when you run: rc-service radbeeper start
```
```
battery and clock of a served counter need the port itself:
    doas rc-service radbeeper stop
```

The program also looks for `/dev/cu.usbserial*` and does not say so. There is
no `rc-service` and no boot service on a Mac. Whether a GMC-320's adapter
always appears under that name on a Mac, and not as `cu.wchusbserial*`,
could not be checked. *Fix:* choose the words by platform in the three
places; on a Mac say "open RadBeeper again" and name the `cu.` pattern.

**7.3 The service outlives the window, and nothing shows it or stops it
(confuses).** That it keeps logging is the design, and the README says so.
But a Mac user has no icon, no menu and no command for it: the stop command
the program prints is the `rc-service` one. *Fix:* document how to find and
stop it; in time, `radbeeper service --stop`.

**7.4 The installed app was older than the tree and said so nowhere (bug in
the installed copy).** `/Applications/RadBeeper.app`, built at 15:34, with no
counter:

```
no counter: a serial device is present but is not a GMC counter
    Tried . None answered <GETVER>> with a GMC version.
```

The tree's build of 16:02 has this fixed and prints "no serial device is
present". Both call themselves 0.6.0 (1.2). *Fix:* run `make app` again; and
1.2.

### 8. Working in a fleet

**8.1 After the counter is unplugged, `now` still exits 0 with the old
figures (confuses).** Forty seconds after the fake counter was stopped:

```
#serial	cpm	cpm_300	usvh	age	temp_c	temp_age	live	port	version
123456789ABCDE	25.0	36.8	0.2429	40.4	26.1	48.4	no	/dev/ttys003	GMC-320Re 4.26
[exit 0]
```

The ages and `live no` are honest, and a careful caller reads them. A
one-line script that takes the sixth column gets a room temperature from a
counter that is no longer there, for as long as the service runs. *Fix:*
`now --max-age SECONDS`, exiting 3 and printing `-` for anything older; say
in the README that a caller must read `temp_age`.

**8.2 `probe` lists an unplugged counter as held and says nothing else
(confuses).** In the same state `probe` printed the counter, its port and
`held by    the radbeeper serving /tmp/rbux2/sock`, exit 0, while the status
file read `waiting: /dev/ttys003 stopped answering`. *Fix:* print the status
line under a held counter.

**8.3 The README's fleet section does not say the service must be the new
build (confuses).** `now` and the temperature file exist only when the
running service is from this tree. Running it explains that the boot service
runs `/usr/local/bin/radbeeper` and not the copy `make install` wrote; the
fleet section, which is where a fleet user stops reading, does not. What
`now` prints against an older service was not tested. *Fix:* the four-line
upgrade in the fleet section, and a sentence on how to tell.

---

## What could not be reviewed

- **Clicking and typing in the window.** Whether a click on a tile, a paper
  or the thermometer does what the help says, whether pointing at a bar
  shows its time, and how the window behaves while it is resized. Every
  statement about the window above is from a picture.
- **A real counter.** Plugging in and unplugging, a charge-only cable, the
  85.0 °C first answer, the backfill at start, two counters at once, and the
  name a real adapter takes on a Mac.
- **A Linux desktop.** The launcher entries, the icon, `hotplug` opening a
  monitor, the fonts the window gets on Copal, and the `dialout` step.
  `hotplug --duration 3` on this Mac printed nothing and exited 0.
- **A phone.** Headless Chrome on a desktop does not lay a page out narrower
  than about 500 pixels, so the 390-pixel pictures are cropped, not reflowed,
  and say nothing. The pages do carry a viewport tag and one `max-width`
  rule.
- **Hours of use.** The longest any program ran was about twelve minutes. What the
  monitor is like on its third day was not seen.
- **The random numbers and the frame browser.** The fake counter's logs had
  no emissions, so `random.html` and `frames.html` were not written.

---

## The ten changes that would help most

| | Change | Findings | Size |
|---|---|---|---|
| 1 | On "no counter", say what to do next; on a Mac, keep looking for a counter after the app is open | 4.2, 7.1 | small, then medium |
| 2 | A key and click line at the foot of the window | 4.1 | small |
| 3 | Check the command's name before looking for a counter; make `help` work | 2.1 | small |
| 4 | Choose the words by platform: no `rc-service`, no "next boot" and the `cu.` port name on a Mac | 7.2, 7.3 | small |
| 5 | A Mac section in Troubleshooting and in Running it | 6.1 | small |
| 6 | In the README's first run, put `service` before the window and say the window needs it | 1.1 | small |
| 7 | Link the report to the drums page and the front page; a neutral opening paragraph; fix the dead link | 5.1, 5.2, 5.3 | small |
| 8 | Let `cpm` answer from the service, and correct the busy message that says a service never causes it | 2.2 | medium |
| 9 | The window's empty and small states: words in the live drum before its first line, tiles at 560 by 400, no overlap at 420 wide | 1.3, 4.3, 4.4, 4.5 | medium |
| 10 | Tell the tree from the release: a version of its own, or the commit beside it | 1.2, 7.4 | small |

Next after these: the terminal's key line (3.1, 3.2), `export` refusing an
empty directory (2.4), and `now --max-age` (8.1).

---

[← back to the overview](../README.md)
