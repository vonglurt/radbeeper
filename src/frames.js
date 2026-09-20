/* SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Paul Richeson
 *
 * The frame viewer: every second of decay that went into a random line, in a
 * table you can sort, filter, and scrub through.
 *
 * ONE COPY, TWO EMBEDDERS. This file is compiled into the Rust with
 * include_str! and pasted into the Python between markers by
 * tools/embedjs.py, and tests/test_frames_js.py fails if the two ever differ.
 * That is what lets `random.html` carry a frame viewer while ./radbeeper --
 * which has never heard of a frame -- still produces the page byte for byte:
 * both implementations copy these characters without reading them, and the
 * decoding happens here, in the browser, once.
 *
 * ZERO DEPENDENCIES, AND NO FETCH FOR THE DEFAULT VIEW. The recent frames are
 * embedded in the page as base64, so a random.html saved to a disk and opened
 * off file:// still shows its data -- which matters, because the page is an
 * audit trail and an audit trail that only works while a web server is up is
 * not much of one. Older months are fetched on demand and simply stay
 * unavailable when there is nothing to fetch from.
 */
(function () {
  "use strict";

  // ------------------------------------------------------------- reading ---

  /** The JSON or text in a <script> the page embedded, or null. */
  function payload(id) {
    var el = document.getElementById(id);
    return el ? el.textContent : null;
  }

  /** base64 -> Uint8Array, without assuming the string is clean. */
  function unbase64(text) {
    var clean = (text || "").replace(/[^A-Za-z0-9+/=]/g, "");
    if (!clean) return new Uint8Array(0);
    var raw;
    try {
      raw = atob(clean);
    } catch (e) {
      return new Uint8Array(0);
    }
    var out = new Uint8Array(raw.length);
    for (var i = 0; i < raw.length; i++) out[i] = raw.charCodeAt(i) & 0xff;
    return out;
  }

  var MAGIC_V1 = [0x52, 0x42, 0x46, 0x31]; // "RBF1"
  var MAGIC_V2 = [0x52, 0x42, 0x46, 0x32]; // "RBF2"
  var ESCAPE = 0xfe;

  function magicAt(buf, at) {
    if (at + 4 > buf.length) return 0;
    var m1 = true, m2 = true;
    for (var i = 0; i < 4; i++) {
      if (buf[at + i] !== MAGIC_V1[i]) m1 = false;
      if (buf[at + i] !== MAGIC_V2[i]) m2 = false;
    }
    return m2 ? 2 : m1 ? 1 : 0;
  }

  function hex(buf, at, n) {
    var s = "";
    for (var i = 0; i < n; i++) {
      var b = buf[at + i];
      s += (b >> 4).toString(16) + (b & 15).toString(16);
    }
    return s;
  }

  /**
   * One frame at `at`, or null.
   *
   * A PORT OF Frame::decode, AND IT HAS TO STAY ONE. The escape rule, the
   * inline-count rule and the "a length that cannot fit is damage" rule are
   * the same three rules in the same order; src/entropy.rs is the
   * specification and this is the second implementation of it.
   */
  function decodeFrame(buf, at) {
    var version = magicAt(buf, at);
    if (!version || at + 5 > buf.length) return null;
    var p = at + 4;
    var suspect = buf[p++] !== 0;

    var varint = function () {
      var v = 0, shift = 0;
      for (;;) {
        if (p >= buf.length) return null;
        var b = buf[p++];
        v += (b & 0x7f) * Math.pow(2, shift);
        if (!(b & 0x80)) return v;
        shift += 7;
        if (shift > 56) return null;
      }
    };

    var seq = varint(); if (seq === null) return null;
    var started = varint(); if (started === null) return null;
    var n = varint(); if (n === null || n > buf.length) return null;

    var samples = new Array(n);
    for (var i = 0; i < n; i++) {
      if (p >= buf.length) return null;
      var tag = buf[p++];
      if (tag === ESCAPE) {
        var gap = varint(); if (gap === null) return null;
        var count = varint(); if (count === null) return null;
        samples[i] = [gap, count];
      } else if (tag === 0xff) {
        return null;
      } else {
        samples[i] = [1, tag];
      }
    }
    var klen = varint();
    if (klen === null || klen > buf.length - p) return null;
    var key = hex(buf, p, klen);
    p += klen;
    var link = "";
    if (version === 2) {
      var llen = varint();
      if (llen === null || llen > buf.length - p) return null;
      link = hex(buf, p, llen);
      p += llen;
    }
    return { seq: seq, started: started, suspect: suspect, samples: samples,
             key: key, link: link, end: p };
  }

  /**
   * Every frame in a buffer, damaged ones skipped.
   *
   * A decode is not enough; it has to LAND somewhere. Corrupting a length
   * inside a frame does not make it fail to parse, it makes it parse as a
   * different frame that ends in the middle of the next one -- so a frame is
   * only accepted when what follows is another magic or the end.
   */
  function readFrames(buf) {
    var out = [], at = 0;
    while (at + 4 <= buf.length) {
      var f = decodeFrame(buf, at);
      if (f && f.end > at && (f.end === buf.length || magicAt(buf, f.end))) {
        out.push(f);
        at = f.end;
      } else {
        at++;
        while (at + 4 <= buf.length && !magicAt(buf, at)) at++;
      }
    }
    return out;
  }

  // ---------------------------------------------------------------- bands ---
  //
  // The same five names, the same five floors and the same five colours as
  // the dial in radbeeper-gui and the legend on the monitor page. A viewer
  // that invented its own thresholds would be a third opinion about what
  // counts as a warning, which is the one thing a instrument must not have.

  var BANDS = [
    { name: "attenuated", floor: 0,   hue: "#739ed9" },
    { name: "nominal",    floor: 30,  hue: "#66d98c" },
    { name: "advisory",   floor: 120, hue: "#e8d24a" },
    { name: "warning",    floor: 240, hue: "#fa9e40" },
    { name: "deadly",     floor: 600, hue: "#e5484d" }
  ];

  /// Emissions dated before this were written by a build that put
  /// time.monotonic() where the wall clock belonged, so they date themselves
  /// to 1969. The audit page has always printed "not kept" for those rows
  /// rather than the date, and the viewer uses the same cut.
  ///
  /// It matters more here than there. A record holding five bad rows and five
  /// good ones spans fifty-seven years; every real bar lands in the last
  /// pixel and the range selector is a blank box. So a stale row is treated
  /// as UNDATED: it gets no vote on the scale, and -- because an undated row
  /// is exempt from the range filter -- it is still listed, still counted,
  /// and still chain-checked. It is evidence, not noise.
  var EPOCH_FLOOR = new Date(2000, 0, 1).getTime() / 1000;

  function bandOf(cpm) {
    for (var i = BANDS.length - 1; i >= 0; i--) {
      if (cpm >= BANDS[i].floor) return BANDS[i];
    }
    return BANDS[0];
  }

  // ------------------------------------------------------------- the data ---

  function parseTsv(text) {
    var rows = [], lines = (text || "").split("\n");
    var head = null;
    for (var i = 0; i < lines.length; i++) {
      var line = lines[i];
      if (!line) continue;
      if (line.charAt(0) === "#") {
        // THE HEADER IS THE LAST `#` LINE WITH TABS IN IT. The file opens
        // with a few lines of prose -- what counter, where it was, what the
        // clamp means -- and taking the first `#` line as the header read the
        // prose as column names and then dropped every row for being too
        // short. Comments have no tabs; the header is nothing but tabs.
        if (line.indexOf("\t") >= 0) head = line.slice(1).split("\t");
        continue;
      }
      var c = line.split("\t");
      if (!head || c.length < head.length) continue;
      var row = {};
      for (var k = 0; k < head.length; k++) row[head[k]] = c[k];
      rows.push(row);
    }
    return rows;
  }

  function num(v) {
    if (v === undefined || v === null || v === "" || v === "--") return null;
    var n = parseFloat(v);
    return isFinite(n) ? n : null;
  }

  /** "2026-09-14T22:13:20" -> epoch seconds, read as LOCAL time. */
  function stampToEpoch(s) {
    var m = /^(\d{4})-(\d\d)-(\d\d)[T ](\d\d):(\d\d):(\d\d)/.exec(s || "");
    if (!m) return null;
    return new Date(+m[1], +m[2] - 1, +m[3], +m[4], +m[5], +m[6]).getTime() / 1000;
  }

  function pad(n) { return (n < 10 ? "0" : "") + n; }

  function stamp(epoch) {
    var d = new Date(epoch * 1000);
    return d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate())
      + " " + pad(d.getHours()) + ":" + pad(d.getMinutes()) + ":" + pad(d.getSeconds());
  }

  function shortStamp(epoch) {
    var d = new Date(epoch * 1000);
    return pad(d.getMonth() + 1) + "-" + pad(d.getDate()) + " " + pad(d.getHours())
      + ":" + pad(d.getMinutes());
  }

  function fmt(v, places) {
    if (v === null || v === undefined || !isFinite(v)) return "--";
    return v.toFixed(places === undefined ? 2 : places);
  }

  // --------------------------------------------------------------- sha256 ---
  //
  // WHY NOT crypto.subtle. It is missing outside a secure context, and
  // file:// is not one in every browser -- which is exactly the case this
  // page is meant to survive, since the point of embedding the frames is that
  // a saved copy still audits. It is also asynchronous, and a chain check
  // that has to be awaited turns every render path into a promise. Sixty
  // lines of FIPS 180-4 costs less than either.

  var K256 = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1,
    0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3,
    0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174, 0xe49b69c1, 0xefbe4786,
    0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147,
    0x06ca6351, 0x14292967, 0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13,
    0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b,
    0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a,
    0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208,
    0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2
  ];

  function sha256(bytes) {
    var h = [0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
             0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19];
    var len = bytes.length;
    var padded = new Uint8Array((((len + 8) >> 6) + 1) << 6);
    padded.set(bytes);
    padded[len] = 0x80;
    // The length in bits, big-endian, in the last eight bytes. Lengths here
    // are tens of bytes, so the high word is always zero -- but it is written
    // rather than assumed, because a viewer is a thing people paste elsewhere.
    var bits = len * 8;
    var dv = new DataView(padded.buffer);
    dv.setUint32(padded.length - 8, Math.floor(bits / 4294967296));
    dv.setUint32(padded.length - 4, bits >>> 0);

    var w = new Int32Array(64);
    for (var off = 0; off < padded.length; off += 64) {
      for (var i = 0; i < 16; i++) w[i] = dv.getInt32(off + i * 4);
      for (i = 16; i < 64; i++) {
        var s0 = ((w[i - 15] >>> 7) | (w[i - 15] << 25))
               ^ ((w[i - 15] >>> 18) | (w[i - 15] << 14)) ^ (w[i - 15] >>> 3);
        var s1 = ((w[i - 2] >>> 17) | (w[i - 2] << 15))
               ^ ((w[i - 2] >>> 19) | (w[i - 2] << 13)) ^ (w[i - 2] >>> 10);
        w[i] = (w[i - 16] + s0 + w[i - 7] + s1) | 0;
      }
      var a = h[0], b = h[1], c = h[2], d = h[3];
      var e = h[4], f = h[5], g = h[6], hh = h[7];
      for (i = 0; i < 64; i++) {
        var S1 = ((e >>> 6) | (e << 26)) ^ ((e >>> 11) | (e << 21))
               ^ ((e >>> 25) | (e << 7));
        var ch = (e & f) ^ (~e & g);
        var t1 = (hh + S1 + ch + K256[i] + w[i]) | 0;
        var S0 = ((a >>> 2) | (a << 30)) ^ ((a >>> 13) | (a << 19))
               ^ ((a >>> 22) | (a << 10));
        var maj = (a & b) ^ (a & c) ^ (b & c);
        var t2 = (S0 + maj) | 0;
        hh = g; g = f; f = e; e = (d + t1) | 0;
        d = c; c = b; b = a; a = (t1 + t2) | 0;
      }
      h[0] = (h[0] + a) | 0; h[1] = (h[1] + b) | 0;
      h[2] = (h[2] + c) | 0; h[3] = (h[3] + d) | 0;
      h[4] = (h[4] + e) | 0; h[5] = (h[5] + f) | 0;
      h[6] = (h[6] + g) | 0; h[7] = (h[7] + hh) | 0;
    }
    var out = "";
    for (i = 0; i < 8; i++) out += ("00000000" + (h[i] >>> 0).toString(16)).slice(-8);
    return out;
  }

  function fromHex(text) {
    var n = Math.floor((text || "").length / 2);
    var out = new Uint8Array(n);
    for (var i = 0; i < n; i++) out[i] = parseInt(text.substr(i * 2, 2), 16) & 0xff;
    return out;
  }

  function concatBytes(parts) {
    var n = 0, i;
    for (i = 0; i < parts.length; i++) n += parts[i].length;
    var out = new Uint8Array(n), at = 0;
    for (i = 0; i < parts.length; i++) { out.set(parts[i], at); at += parts[i].length; }
    return out;
  }

  var CHAIN_LABEL = new Uint8Array(
    [114, 97, 100, 98, 101, 101, 112, 101, 114, 47, 99, 104, 97, 105, 110, 47, 49]
  ); // "radbeeper/chain/1"

  var GENESIS = "0000000000000000000000000000000000000000000000000000000000000000";

  /** One step of the chain: H(label || prev || key). Mirrors entropy::chain. */
  function chainStep(prev, key) {
    return sha256(concatBytes([CHAIN_LABEL, fromHex(prev), fromHex(key)]));
  }

  /**
   * Walk the emissions in order and mark each one's chain state.
   *
   * `ok` means this row's stored link is the one its key and its predecessor
   * imply. `broken` means it is not -- which, on a row whose own key still
   * recomputes, means something was removed or reordered rather than edited.
   * A row with no stored link at all (written before 0.5) is `absent`: not a
   * failure, just a stretch of record the chain does not cover.
   */
  function verifyChain(rows) {
    var prev = GENESIS, seen = false;
    for (var i = 0; i < rows.length; i++) {
      var r = rows[i];
      if (!r.key) { r.chain = "absent"; continue; }
      var want = chainStep(prev, r.key);
      if (!r.link) {
        r.chain = "absent";
        // An unlinked row still advances the chain, so the rows after it are
        // judged against where the record actually is rather than against a
        // chain that silently stopped.
        prev = want;
        continue;
      }
      r.chain = r.link === want ? "ok" : "broken";
      if (r.chain === "broken") {
        // Re-seat on what the file claims, so ONE bad link reports as one bad
        // link instead of painting every row after it red.
        prev = r.link;
      } else {
        prev = want;
      }
      seen = true;
    }
    return seen;
  }

  // ---------------------------------------------------------- the joining ---

  /**
   * One row per emission: what the .tsv recorded, plus the frame's seconds
   * when the frame for it is here.
   *
   * THE TABLE IS DRIVEN BY THE TSV AND NOT BY THE FRAMES, which is what keeps
   * the page honest about its own gaps. Frames are embedded up to a budget,
   * so the older end of the record has rows with no frame behind them; those
   * rows still appear, still carry their key and their chain link, and simply
   * cannot be expanded. A viewer built the other way round would quietly show
   * only the part of the history it happened to be carrying.
   */
  function build(meta, tsvText, frameBytes) {
    var rows = parseTsv(tsvText);
    var frames = readFrames(frameBytes);
    var bySeq = {};
    for (var i = 0; i < frames.length; i++) {
      // Sequence numbers restart per counter, and a page is per counter, so
      // seq is unique here. Later wins: a re-run that rewrote a frame is
      // showing its newer self.
      bySeq[frames[i].seq] = frames[i];
    }
    var out = [];
    for (i = 0; i < rows.length; i++) {
      var r = rows[i];
      var started = stampToEpoch(r.started || r.time);
      var stale = started !== null && started < EPOCH_FLOOR;
      if (stale) started = null;
      // A pool's span in samples IS its span in seconds -- one count a
      // second is the whole of what the protocol offers -- so there is one
      // column and not two that could disagree.
      var seconds = num(r.samples) || num(r.seconds) || 0;
      var counts = num(r.counts);
      var cpm = num(r.cpm);
      if (cpm === null && counts !== null && seconds > 0) cpm = counts * 60 / seconds;
      var f = bySeq[num(r.seq)];
      out.push({
        seq: num(r.seq),
        started: started,
        seconds: seconds,
        samples: num(r.samples),
        counts: counts,
        cpm: cpm,
        usvh: num(r.usvh),
        bits: num(r.bits),
        flat: (r.flat || "") === "yes",
        stale: stale,
        key: (r.key || r.hex || "").toLowerCase(),
        link: (r.link || "").toLowerCase(),
        cpm30: num(r.cpm30),
        cpm300: num(r.cpm300),
        frame: f || null,
        chain: "absent"
      });
    }
    // VERIFIED IN FILE ORDER, WHICH IS THE ORDER THE CHAIN WAS BUILT IN.
    //
    // This used to sort by `seq` first, on the assumption that a sequence
    // number counts the record. It does not: `seq` is the POOL's counter and
    // it restarts -- at a service restart, and separately for each tube -- so
    // a real month of emissions reads 0 0 1 0 0 0 0 1 2 99. Sorting by it
    // shuffles the rows out of the order their links were computed in and
    // reports chain breaks in a chain that is perfectly sound. The first page
    // rendered with real data claimed three.
    //
    // The table's own sorting is a separate thing entirely and happens in
    // `visible()`, after every row already knows its chain state.
    verifyChain(out);
    for (i = 0; i < out.length; i++) {
      out[i].band = bandOf(out[i].cpm === null ? 0 : out[i].cpm);
      out[i].verified = out[i].frame ? frameKeyMatches(out[i]) : null;
    }
    return { rows: out, frames: frames, meta: meta };
  }

  /**
   * Does the frame's seconds still add up to the count the .tsv recorded?
   *
   * NOT THE KEY ITSELF, deliberately. Recomputing the key means packing the
   * counts to one nibble each and hashing them with the label and the start
   * second -- doable here, and it would be a second implementation of the
   * digest in a language with no integer type, which is how two
   * implementations quietly stop agreeing. `radbeeper random --check` and
   * `--frames` do that properly, in the same code that writes it. What this
   * checks is the weaker, useful thing: the raw seconds and the summary row
   * describe the same stretch of time.
   */
  function frameKeyMatches(row) {
    var f = row.frame;
    if (!f) return null;
    if (f.key && row.key && f.key !== row.key) return false;
    var total = 0;
    for (var i = 0; i < f.samples.length; i++) total += f.samples[i][1];
    if (row.samples !== null && f.samples.length !== row.samples) return false;
    // The .tsv's counts column is clamped at fifteen a second, so it is a
    // floor on the frame's total and never an equality.
    if (row.counts !== null && total < row.counts) return false;
    return true;
  }

  // ------------------------------------------------------------------ dom ---

  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = String(text);
    return e;
  }

  var STYLE = [
    ".rb { --rb-line:#d8d2c4; --rb-dim:#6d6a63; --rb-bg:#fbf9f4; --rb-alt:#f3efe6;",
    "      font: 13px/1.45 ui-monospace,Menlo,Consolas,monospace; margin:1.5rem 0; }",
    "@media (prefers-color-scheme: dark) { .rb {",
    "  --rb-line:#33383f; --rb-dim:#8b9099; --rb-bg:#15181c; --rb-alt:#1b1f24; } }",
    ".rb-bar { display:flex; flex-wrap:wrap; gap:.5rem 1.2rem; align-items:center;",
    "          margin-bottom:.6rem; }",
    ".rb-bar label { color:var(--rb-dim); display:inline-flex; gap:.35rem;",
    "                align-items:center; }",
    ".rb input, .rb select, .rb button { font:inherit; padding:.2rem .4rem;",
    "  border:1px solid var(--rb-line); background:var(--rb-bg); color:inherit;",
    "  border-radius:3px; }",
    ".rb button { cursor:pointer; }",
    ".rb button:hover { border-color:var(--rb-dim); }",
    ".rb-range { width:100%; height:92px; display:block; touch-action:none;",
    "            border:1px solid var(--rb-line); border-radius:3px;",
    "            background:var(--rb-bg); cursor:crosshair; }",
    ".rb-note { color:var(--rb-dim); margin:.4rem 0 .8rem; }",
    ".rb-wrap { overflow-x:auto; border:1px solid var(--rb-line); border-radius:3px; }",
    ".rb-table { border-collapse:collapse; width:100%; font-size:12px; }",
    ".rb-table th, .rb-table td { padding:.25rem .5rem; text-align:right;",
    "  white-space:nowrap; border-bottom:1px solid var(--rb-line); }",
    ".rb-table th { position:sticky; top:0; background:var(--rb-alt);",
    "  cursor:pointer; user-select:none; z-index:1; }",
    ".rb-table th.rb-sorted::after { content:' \\25BE'; }",
    ".rb-table th.rb-sorted.rb-asc::after { content:' \\25B4'; }",
    ".rb-table td.rb-l, .rb-table th.rb-l { text-align:left; }",
    ".rb-row { cursor:pointer; }",
    ".rb-row:hover td { background:var(--rb-alt); }",
    ".rb-key { font-size:11px; color:var(--rb-dim); }",
    ".rb-chip { display:inline-block; padding:0 .4rem; border-radius:2px;",
    "  color:#101214; font-weight:600; font-size:11px; }",
    ".rb-heat { position:relative; }",
    ".rb-heat span { position:relative; z-index:1; }",
    ".rb-heat i { position:absolute; left:0; top:2px; bottom:2px; z-index:0;",
    "  opacity:.28; border-radius:2px; }",
    ".rb-bad { color:#e5484d; font-weight:600; }",
    ".rb-good { color:#3a9d5d; }",
    ".rb-detail td { background:var(--rb-alt); }",
    ".rb-secs { display:flex; flex-wrap:wrap; gap:2px; margin:.4rem 0; }",
    ".rb-sec { width:22px; text-align:center; border-radius:2px; font-size:11px;",
    "  color:#101214; }",
    ".rb-empty { padding:1.2rem; text-align:center; color:var(--rb-dim); }",
    ".rb-dl a { margin-right:.8rem; }"
  ].join("\n");

  var SVGNS = "http://www.w3.org/2000/svg";
  function svg(tag, attrs) {
    var e = document.createElementNS(SVGNS, tag);
    for (var k in attrs) if (attrs.hasOwnProperty(k)) e.setAttribute(k, attrs[k]);
    return e;
  }

  var COLUMNS = [
    { key: "seq",     label: "seq",     align: "r", fmt: function (r) { return r.seq; } },
    { key: "started", label: "started", align: "l",
      fmt: function (r) {
        if (r.started) return stamp(r.started);
        // The same two words the audit page uses for these rows, so a reader
        // moving between the table and the page is not told two stories.
        return r.stale ? "not kept" : "--";
      } },
    { key: "seconds", label: "span s",  align: "r", fmt: function (r) { return r.seconds; } },
    { key: "samples", label: "samples", align: "r",
      fmt: function (r) { return r.samples === null ? "--" : r.samples; } },
    { key: "counts",  label: "counts",  align: "r",
      fmt: function (r) { return r.counts === null ? "--" : r.counts; } },
    { key: "cpm",     label: "CPM",     align: "r", heat: true,
      fmt: function (r) { return fmt(r.cpm, 1); } },
    { key: "usvh",    label: "uSv/h",   align: "r",
      fmt: function (r) { return fmt(r.usvh, 4); } },
    { key: "bits",    label: "bits/s",  align: "r",
      fmt: function (r) { return fmt(r.bits, 3); } },
    { key: "band",    label: "band",    align: "l", band: true,
      fmt: function (r) { return r.band.name; } },
    { key: "chain",   label: "chain",   align: "l", chain: true,
      fmt: function (r) { return r.chain; } },
    { key: "key",     label: "key",     align: "l", mono: true,
      fmt: function (r) { return r.key ? r.key.slice(0, 16) + "…" : "--"; } }
  ];

  function mount(model, host) {
    var rows = model.rows;
    var span = extent(rows);
    var state = {
      lo: span.lo, hi: span.hi,
      sort: { key: "seq", asc: false },
      bands: null,          // null = every band
      minCpm: null, maxCpm: null,
      search: "",
      chainOnly: false,
      framesOnly: false,
      open: {}
    };

    var style = el("style");
    style.textContent = STYLE;
    host.appendChild(style);
    var root = el("div", "rb");
    host.appendChild(root);

    var summary = el("p", "rb-note");
    root.appendChild(summary);

    var chart = svg("svg", { "class": "rb-range", preserveAspectRatio: "none" });
    root.appendChild(chart);
    var hint = el("p", "rb-note",
      "Drag across the chart to select a span; drag the edges to resize, the "
      + "middle to pan, scroll to zoom. Double-click to show everything.");
    root.appendChild(hint);

    var bar = el("div", "rb-bar");
    root.appendChild(bar);
    var stats = el("p", "rb-note");
    root.appendChild(stats);
    var wrap = el("div", "rb-wrap");
    root.appendChild(wrap);

    // ------------------------------------------------------------ toolbar ---
    function field(label, node) {
      var l = el("label", null, label);
      l.appendChild(node);
      bar.appendChild(l);
      return node;
    }
    var fromBox = field("from ", el("input"));
    var toBox = field("to ", el("input"));
    fromBox.type = toBox.type = "date";
    var minBox = field("CPM ≥ ", el("input"));
    var maxBox = field("≤ ", el("input"));
    minBox.type = maxBox.type = "number";
    minBox.step = maxBox.step = "any";
    minBox.style.width = maxBox.style.width = "6em";
    var bandBox = field("band ", el("select"));
    bandBox.appendChild(new Option("all", ""));
    for (var bi = 0; bi < BANDS.length; bi++) {
      bandBox.appendChild(new Option(BANDS[bi].name, BANDS[bi].name));
    }
    var searchBox = field("key ", el("input"));
    searchBox.type = "search";
    searchBox.placeholder = "hex prefix";
    searchBox.style.width = "9em";
    var chainBox = field("chain issues only ", el("input"));
    chainBox.type = "checkbox";
    var framesBox = field("with frames only ", el("input"));
    framesBox.type = "checkbox";
    var reset = el("button", null, "reset");
    bar.appendChild(reset);
    var dl = el("button", null, "download shown as TSV");
    bar.appendChild(dl);

    function readToolbar() {
      state.minCpm = num(minBox.value);
      state.maxCpm = num(maxBox.value);
      state.bands = bandBox.value ? bandBox.value : null;
      state.search = (searchBox.value || "").trim().toLowerCase();
      state.chainOnly = chainBox.checked;
      state.framesOnly = framesBox.checked;
      var f = fromBox.value ? stampToEpoch(fromBox.value + "T00:00:00") : null;
      var t = toBox.value ? stampToEpoch(toBox.value + "T23:59:59") : null;
      if (f !== null) state.lo = f;
      if (t !== null) state.hi = t;
      draw();
    }
    [minBox, maxBox, searchBox].forEach(function (n) {
      n.addEventListener("input", readToolbar);
    });
    [bandBox, chainBox, framesBox, fromBox, toBox].forEach(function (n) {
      n.addEventListener("change", readToolbar);
    });
    reset.addEventListener("click", function () {
      state.lo = span.lo; state.hi = span.hi;
      minBox.value = maxBox.value = searchBox.value = "";
      fromBox.value = toBox.value = "";
      bandBox.value = ""; chainBox.checked = framesBox.checked = false;
      state.minCpm = state.maxCpm = state.bands = null;
      state.search = ""; state.chainOnly = state.framesOnly = false;
      draw();
    });
    dl.addEventListener("click", function () { download(visible(), model.meta); });

    // -------------------------------------------------------- the filter ---
    function visible() {
      var out = [];
      for (var i = 0; i < rows.length; i++) {
        var r = rows[i];
        if (r.started !== null && (r.started < state.lo || r.started > state.hi)) continue;
        if (state.minCpm !== null && (r.cpm === null || r.cpm < state.minCpm)) continue;
        if (state.maxCpm !== null && (r.cpm === null || r.cpm > state.maxCpm)) continue;
        if (state.bands && r.band.name !== state.bands) continue;
        if (state.search && r.key.indexOf(state.search) !== 0
            && r.link.indexOf(state.search) !== 0) continue;
        if (state.chainOnly && r.chain === "ok") continue;
        if (state.framesOnly && !r.frame) continue;
        out.push(r);
      }
      var k = state.sort.key, sign = state.sort.asc ? 1 : -1;
      out.sort(function (a, b) {
        var x = a[k], y = b[k];
        if (k === "band") { x = a.band.floor; y = b.band.floor; }
        if (x === null || x === undefined) x = -Infinity;
        if (y === null || y === undefined) y = -Infinity;
        if (typeof x === "string" || typeof y === "string") {
          return sign * String(x).localeCompare(String(y));
        }
        return sign * (x - y);
      });
      return out;
    }

    // ------------------------------------------------------- the brush ---
    //
    // A RANGE SELECTOR OVER TIME, not over row number. The record is not
    // evenly spaced -- a counter that was unplugged for a fortnight leaves a
    // fortnight-shaped hole -- and a selector indexed by row would make that
    // hole invisible by squeezing it to nothing.

    function px(t, w) {
      if (span.hi <= span.lo) return 0;
      return (t - span.lo) / (span.hi - span.lo) * w;
    }
    function un(x, w) {
      if (w <= 0) return span.lo;
      return span.lo + (x / w) * (span.hi - span.lo);
    }

    function drawRange() {
      while (chart.firstChild) chart.removeChild(chart.firstChild);
      var w = chart.clientWidth || chart.parentNode.clientWidth || 900;
      var h = 92, top = 8, bot = h - 16;
      chart.setAttribute("viewBox", "0 0 " + w + " " + h);

      var peak = 1;
      for (var i = 0; i < rows.length; i++) {
        if (rows[i].cpm !== null && rows[i].cpm > peak) peak = rows[i].cpm;
      }
      // One bar per emission, at least a pixel wide so a single reading in a
      // month-long record is still something you can aim at.
      for (i = 0; i < rows.length; i++) {
        var r = rows[i];
        if (r.started === null || r.cpm === null) continue;
        var x = px(r.started, w);
        var bh = Math.max(1, (r.cpm / peak) * (bot - top));
        var inSel = r.started >= state.lo && r.started <= state.hi;
        chart.appendChild(svg("rect", {
          x: x, y: bot - bh, width: 1.6, height: bh,
          fill: r.band.hue, opacity: inSel ? 0.95 : 0.25
        }));
      }
      // The unselected ends, shaded.
      var lx = px(state.lo, w), hx = px(state.hi, w);
      chart.appendChild(svg("rect", { x: 0, y: 0, width: Math.max(0, lx),
        height: h, fill: "currentColor", opacity: 0.12 }));
      chart.appendChild(svg("rect", { x: hx, y: 0, width: Math.max(0, w - hx),
        height: h, fill: "currentColor", opacity: 0.12 }));
      [lx, hx].forEach(function (x) {
        chart.appendChild(svg("rect", { x: x - 2, y: 0, width: 4, height: h,
          fill: "currentColor", opacity: 0.55 }));
      });
      var lab = svg("text", { x: 4, y: h - 4, fill: "currentColor",
        "font-size": "10", opacity: 0.7 });
      lab.textContent = shortStamp(state.lo) + "  →  " + shortStamp(state.hi);
      chart.appendChild(lab);
    }

    var drag = null;
    chart.addEventListener("pointerdown", function (ev) {
      var w = chart.clientWidth || 900;
      var x = ev.clientX - chart.getBoundingClientRect().left;
      var lx = px(state.lo, w), hx = px(state.hi, w);
      var mode = Math.abs(x - lx) < 6 ? "lo"
               : Math.abs(x - hx) < 6 ? "hi"
               : (x > lx && x < hx) ? "pan" : "new";
      if (mode === "new") {
        state.lo = state.hi = un(x, w);
        mode = "hi";
      }
      drag = { mode: mode, w: w, x: x, lo: state.lo, hi: state.hi };
      chart.setPointerCapture(ev.pointerId);
      ev.preventDefault();
    });
    chart.addEventListener("pointermove", function (ev) {
      if (!drag) return;
      var x = ev.clientX - chart.getBoundingClientRect().left;
      var t = un(x, drag.w);
      if (drag.mode === "lo") state.lo = Math.min(t, state.hi);
      else if (drag.mode === "hi") state.hi = Math.max(t, state.lo);
      else {
        var shift = un(x, drag.w) - un(drag.x, drag.w);
        var width = drag.hi - drag.lo;
        state.lo = Math.max(span.lo, Math.min(span.hi - width, drag.lo + shift));
        state.hi = state.lo + width;
      }
      clampSel();
      draw();
    });
    function endDrag(ev) {
      if (!drag) return;
      drag = null;
      try { chart.releasePointerCapture(ev.pointerId); } catch (e) { /* gone */ }
    }
    chart.addEventListener("pointerup", endDrag);
    chart.addEventListener("pointercancel", endDrag);
    chart.addEventListener("dblclick", function () {
      state.lo = span.lo; state.hi = span.hi; draw();
    });
    chart.addEventListener("wheel", function (ev) {
      var w = chart.clientWidth || 900;
      var at = un(ev.clientX - chart.getBoundingClientRect().left, w);
      var k = ev.deltaY > 0 ? 1.25 : 0.8;
      state.lo = at - (at - state.lo) * k;
      state.hi = at + (state.hi - at) * k;
      clampSel();
      draw();
      ev.preventDefault();
    }, { passive: false });

    function clampSel() {
      if (state.lo < span.lo) state.lo = span.lo;
      if (state.hi > span.hi) state.hi = span.hi;
      // A selection narrower than a second selects nothing and cannot be
      // dragged back open, so it has a floor.
      if (state.hi - state.lo < 1) state.hi = state.lo + 1;
    }

    // -------------------------------------------------------- the table ---

    function drawTable(shown) {
      while (wrap.firstChild) wrap.removeChild(wrap.firstChild);
      if (!shown.length) {
        wrap.appendChild(el("div", "rb-empty", "No emissions match these filters."));
        return;
      }
      var table = el("table", "rb-table");
      var thead = el("thead"), tr = el("tr");
      COLUMNS.forEach(function (col) {
        var th = el("th", col.align === "l" ? "rb-l" : null, col.label);
        if (state.sort.key === col.key) {
          th.className = (th.className ? th.className + " " : "")
            + "rb-sorted" + (state.sort.asc ? " rb-asc" : "");
        }
        th.addEventListener("click", function () {
          if (state.sort.key === col.key) state.sort.asc = !state.sort.asc;
          else { state.sort.key = col.key; state.sort.asc = false; }
          draw();
        });
        tr.appendChild(th);
      });
      thead.appendChild(tr);
      table.appendChild(thead);

      var peak = 1;
      shown.forEach(function (r) { if (r.cpm > peak) peak = r.cpm; });

      var tbody = el("tbody");
      // A CAP ON RENDERED ROWS, not on matched ones. Ten thousand <tr> is
      // where a browser starts to feel it, and the honest response is to say
      // how many are being withheld and let the range selector do its job.
      var LIMIT = 2000;
      shown.slice(0, LIMIT).forEach(function (r) {
        var row = el("tr", "rb-row");
        COLUMNS.forEach(function (col) {
          var td = el("td", col.align === "l" ? "rb-l" : null);
          var text = col.fmt(r);
          if (col.band) {
            var chip = el("span", "rb-chip", text);
            chip.style.background = r.band.hue;
            td.appendChild(chip);
          } else if (col.heat && r.cpm !== null) {
            td.className += " rb-heat";
            var fill = el("i");
            fill.style.background = r.band.hue;
            fill.style.width = Math.max(2, (r.cpm / peak) * 100) + "%";
            td.appendChild(fill);
            td.appendChild(el("span", null, text));
          } else if (col.chain) {
            td.className += r.chain === "broken" ? " rb-bad"
                          : r.chain === "ok" ? " rb-good" : "";
            td.textContent = text;
          } else if (col.mono) {
            td.className += " rb-key";
            td.textContent = text;
          } else {
            td.textContent = text;
          }
          row.appendChild(td);
        });
        row.addEventListener("click", function () {
          state.open[r.seq] = !state.open[r.seq];
          draw();
        });
        tbody.appendChild(row);
        if (state.open[r.seq]) tbody.appendChild(detail(r));
      });
      table.appendChild(tbody);
      wrap.appendChild(table);
      if (shown.length > LIMIT) {
        wrap.appendChild(el("div", "rb-empty",
          "Showing the first " + LIMIT + " of " + shown.length
          + " -- narrow the range to see the rest."));
      }
    }

    function detail(r) {
      var tr = el("tr", "rb-detail");
      var td = el("td");
      td.colSpan = COLUMNS.length;
      td.appendChild(el("div", null, "key  " + (r.key || "--")));
      td.appendChild(el("div", null, "link " + (r.link || "(before the chain)")));
      if (!r.frame) {
        td.appendChild(el("div", "rb-note",
          "The raw seconds for this emission are not in this page. "
          + "They are in the month's .bin file, linked above."));
      } else {
        var f = r.frame;
        var grid = el("div", "rb-secs");
        var at = f.started;
        for (var i = 0; i < f.samples.length; i++) {
          at += f.samples[i][0];
          var c = f.samples[i][1];
          var cell = el("div", "rb-sec", c);
          cell.style.background = bandOf(c * 60).hue;
          cell.title = stamp(at) + " — " + c + " counts ("
                     + (c * 60) + " CPM)";
          grid.appendChild(cell);
          if (f.samples[i][0] > 1) cell.style.outline = "2px solid #e5484d";
        }
        td.appendChild(el("div", "rb-note",
          f.samples.length + " seconds, " + (f.suspect ? "spectrum NOT flat"
          : "spectrum flat") + ". A red outline is a gap -- a second the "
          + "counter was away. Hover for the time."));
        td.appendChild(grid);
      }
      tr.appendChild(td);
      return tr;
    }

    function drawStats(shown) {
      var counts = 0, secs = 0, n = 0, broken = 0, framed = 0;
      shown.forEach(function (r) {
        if (r.counts !== null) counts += r.counts;
        secs += r.seconds || 0;
        if (r.chain === "broken") broken++;
        if (r.frame) framed++;
        n++;
      });
      var mean = secs > 0 ? counts * 60 / secs : null;
      stats.textContent = n + " of " + rows.length + " emissions · "
        + counts + " counts over " + secs + "s · mean "
        + fmt(mean, 1) + " CPM · " + framed + " with raw seconds here"
        + (broken ? " · " + broken + " CHAIN BREAKS" : "");
    }

    function draw() {
      clampSel();
      var shown = visible();
      drawRange();
      drawTable(shown);
      drawStats(shown);
    }

    summary.textContent = model.meta.summary || "";
    var resizeAt = 0;
    window.addEventListener("resize", function () {
      clearTimeout(resizeAt);
      resizeAt = setTimeout(drawRange, 120);
    });
    draw();
  }

  function extent(rows) {
    var lo = null, hi = null, i, t;
    // Stale rows arrive here already undated, from `build`.
    for (i = 0; i < rows.length; i++) {
      t = rows[i].started;
      if (t === null) continue;
      if (lo === null || t < lo) lo = t;
      if (hi === null || t > hi) hi = t;
    }
    if (lo === null) { lo = 0; hi = 1; }
    if (hi <= lo) hi = lo + 1;
    // A little air at each end, so the first and last bars are not half
    // under the handles.
    var pad = (hi - lo) * 0.02;
    return { lo: lo - pad, hi: hi + pad };
  }

  /** What is on screen, as a TSV the browser saves. */
  function download(shown, meta) {
    var head = ["seq", "started", "seconds", "samples", "counts", "cpm",
                "usvh", "bits", "flat", "band", "chain", "key", "link"];
    var lines = ["# radbeeper " + (meta.version || "") + " -- counter "
                 + (meta.serial || "") + (meta.place ? " at " + meta.place : ""),
                 "#" + head.join("\t")];
    shown.forEach(function (r) {
      lines.push([r.seq, r.started ? stamp(r.started) : "", r.seconds,
                  r.samples === null ? "" : r.samples,
                  r.counts === null ? "" : r.counts,
                  r.cpm === null ? "" : r.cpm.toFixed(4),
                  r.usvh === null ? "" : r.usvh.toFixed(6),
                  r.bits === null ? "" : r.bits.toFixed(4),
                  r.flat ? "yes" : "no", r.band.name, r.chain, r.key, r.link
                 ].join("\t"));
    });
    var blob = new Blob([lines.join("\n") + "\n"], { type: "text/tab-separated-values" });
    var a = document.createElement("a");
    a.href = URL.createObjectURL(blob);
    a.download = "radbeeper-" + (meta.serial || "frames") + ".tsv";
    document.body.appendChild(a);
    a.click();
    setTimeout(function () {
      URL.revokeObjectURL(a.href);
      a.parentNode.removeChild(a);
    }, 0);
  }

  // ----------------------------------------------------------------- boot ---

  function boot() {
    var host = document.getElementById("rb-viewer");
    if (!host) return;
    var meta = {};
    try { meta = JSON.parse(payload("rb-meta") || "{}"); } catch (e) { meta = {}; }
    var model = build(meta, payload("rb-joined") || "",
                      unbase64(payload("rb-frames")));
    if (!model.rows.length) {
      host.appendChild(el("p", "rb-note", "No emissions recorded yet."));
      return;
    }
    mount(model, host);
  }

  // The test harness loads this file under node, where there is no document
  // at all -- so booting is conditional on there being a page to boot into.
  if (typeof document !== "undefined") {
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", boot);
    } else {
      boot();
    }
  }

  // Exposed for the test harness, which runs this file under node with no DOM.
  if (typeof module !== "undefined" && module.exports) {
    module.exports = { readFrames: readFrames, sha256: sha256,
                       chainStep: chainStep, verifyChain: verifyChain,
                       parseTsv: parseTsv, bandOf: bandOf, GENESIS: GENESIS };
  }
})();
