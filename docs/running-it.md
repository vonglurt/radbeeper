# Running it

*A lab report on keeping a counter logging unattended: the boot service, one
log directory two programs can both write, hotplug, and publishing the result
without a build step.*

---

## As a boot service

[Copal](https://github.com/vonglurt/copal) — the Alpine distillation this was
written on — carries RadBeeper as stage 10, which installs an OpenRC service and
a udev rule alongside the `linux-lts` kernel that makes the counter visible in
the first place. **Dormant is the normal state**: with no counter plugged in the service writes down why and exits
0 — a stopped service, not a crash loop.

```sh
rc-service radbeeper start        # or just plug the counter in
cat /var/lib/radbeeper/status     # what it is doing, and why
```

**The service runs `/usr/local/bin/radbeeper`, not the one `make install` put
in `~/.cargo/bin`.** Upgrading your own copy leaves the boot service on
whatever was there before, so give it the new build too:

```sh
make build
doas rc-service radbeeper stop
doas install -m 0755 target/release/radbeeper /usr/local/bin/radbeeper
doas rc-service radbeeper start   # reads the flash first, then logs
```

On start it backfills from the counter's flash before appending anything —
`--no-backfill` skips that. If the new build logs a different set of windows
from the old one, it writes a second header line into the month's file
rather than writing five columns under a four-column header; both readers take
the last header above a row as that row's.

`radbeeper hotplug` is the other half: it sits in your desktop session and opens
the monitor when a counter appears — at login if one is already there, and on
plug-in at any point after.

---

## One log for the service and the monitor

The boot service runs as root and `watch` runs as you, and they should write the
same files. Make the directory the `dialout` group's, which you are already in
for the serial port, and have the service create files the group can write:

```sh
doas rc-service radbeeper stop
doas mkdir -p /var/lib/radbeeper
doas sh -c 'cp -p /var/log/radbeeper/*.tsv /var/log/radbeeper/*.hex /var/lib/radbeeper/ 2>/dev/null; true'
doas chown -R root:dialout /var/lib/radbeeper
doas chmod 2775 /var/lib/radbeeper
doas sh -c 'chmod g+w /var/lib/radbeeper/* 2>/dev/null; true'
# the service script: make the directory at start, files group-writable, status in the new place
doas sed -i -e 's|^\tcheckpath -d -m 0755 /var/log/radbeeper$|&\n\tcheckpath -d -m 2775 -o root:dialout /var/lib/radbeeper|' \
            -e 's|^command_background=true$|&\numask=002|' \
            -e 's|/var/log/radbeeper/status|/var/lib/radbeeper/status|g' /etc/init.d/radbeeper
doas rc-service radbeeper start
```

The setgid bit (the `2` in `2775`) makes every file created in the directory
belong to `dialout`, whoever creates it; `umask=002` makes the service's files
group-writable. `service.log` stays in `/var/log` — it is the service's own
output, and losing it at a reboot is what a log is for.

---

## Publishing it

```sh
radbeeper export --logs logs -o index.html
```

`watch` writes both pages into the log directory by itself — after its
backfill, every hour and on quit — so a monitor left open keeps them current.

One self-contained page: a how-to, summary cards, a log-scale plot of counts per
minute by the hour, a by-day table and the latest rows. **No JavaScript, no web
fonts, no CDN** — the chart is SVG the program draws itself, and the full record
is one link away as the file it already lives in.

**`random.html` is written beside it** whenever there are emissions to account
for, and the index links to it. It is the one claim on the front page a reader
cannot check by looking — 256 bits out of decay — so the audit gets its own
page: the counts drawn against the Poisson model that used to be assumed, the
bits accumulating second by second with the target across them, what the serial
link's one-second resolution costs against timestamped arrivals, and every line
emitted with the counts behind it. `--no-random-page` turns it off.

To put it on the web: **fork this repository, copy your `cpm-*.tsv`,
`random-*.tsv` and `sites.tsv` into `logs/`, and push.**
`.github/workflows/pages.yml` rebuilds both pages and commits them back, so
GitHub Pages serves them with no build step. There is nothing to install in the
workflow — the generator is this same file, which is also why the pages cannot
drift from the log format. The native build's `radbeeper export` writes the
same two pages, byte for byte, and `tests/test_differential.py` is what says so.

---

## What the site is made of

GitHub Pages serves this repository's root, and three generators write into it.

| | |
|---|---|
| `index.html` | the landing page, from `README.md` via `tools/landing.py` |
| `docs/*.html` | the lab reports, from `docs/*.md`, by the same renderer |
| `monitor.html` | the counter's report, from `logs/` via `radbeeper export` |
| `random.html` | the entropy audit, written beside it when there are emissions |

**The report used to be `index.html`.** Somebody arriving from a search for
"GMC-320 Plus Linux" landed on a table of counts per minute with nothing on the
page saying what had produced them; it is one click along now, and linked from
the first screen of the landing page.

**Neither page is hand-written**, and that is the same argument twice. The
report is built by the program that reads the counter, so it cannot drift from
the row format. The landing page *is* the README, rendered — so it cannot drift
from the documentation either. A hand-written overview is a second description
of the program that goes stale the first time the first one changes.

```sh
make site          # build all of it
make site-serve    # and look at it before pushing
```

**`radbeeper export` still defaults to `index.html`**, which is the right name
in a directory of your own and the wrong one here — run in the repository root
it would write the counter's report over the landing page. `make site` passes
`-o monitor.html`, and so does the workflow; if you run the export by hand in
this checkout, pass it too.

`sitemap.xml`, `robots.txt` and `.nojekyll` are written alongside. The last one
matters: without it GitHub runs Jekyll over the repository, which silently drops
any file or folder whose name begins with an underscore.

---

## Further reading

- [The log](the-log.md) — what is in the files this is keeping.
- [The stream](the-stream.md) — why a service holding the ports does not stop
  you watching the counter.
- [Troubleshooting](troubleshooting.md) — the four things it can be when
  `probe` finds nothing.

---

MIT License — Copyright (c) 2026 Paul Richeson

---

[← back to the README](../README.md)
