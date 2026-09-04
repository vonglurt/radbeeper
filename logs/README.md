# logs/

Put your own logs here and push: the workflow rebuilds `index.html` from
whatever it finds.

| File | What it is |
|---|---|
| `cpm-<serial>-YYYY-MM.tsv` | one counter, one month, as `radbeeper service` writes them |
| `sites.tsv` | which counter was where, and from when |

Copy them straight out of `/var/log/radbeeper` (or `~/.local/share/radbeeper`
on a machine where that is not writable). They are tab-separated text and
sort chronologically as they stand, so `sort cpm-*.tsv` is one stream.

**Site names, not coordinates.** `sites.tsv` records a *name* for each place a
counter has been. There is nowhere in it to put a latitude, on purpose: the
page built from these files is meant to be published, and a name is what a
reader needs while a decimal fix is a street address for whoever is holding
the counter.
