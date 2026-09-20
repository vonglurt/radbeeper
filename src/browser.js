/* SPDX-License-Identifier: MIT
 * Copyright (c) 2026 Paul Richeson
 *
 * The frame browser: every frame this counter has ever drawn, by month, by
 * day, and then one at a time -- as a strip of seconds or as a spectrum.
 *
 * WHY THERE IS A SECOND DECODER IN THIS REPOSITORY, AND WHY THAT IS NOT AN
 * ACCIDENT. src/frames.js is byte-locked to the copy tools/embedjs.py pastes
 * into the Python program, because the two implementations have to produce
 * `random.html` character for character. This page is written by the Rust
 * alone -- the Python has no frame browser and is not going to grow one -- so
 * a file shared between them would drag the whole parity apparatus along with
 * it for no benefit. The cost is this decoder, forty lines that already exist
 * next door; the price of that cost is tests/test_browser_js.py, which
 * decodes the same fixture with both files and fails if they ever disagree.
 *
 * ZERO DEPENDENCIES, AND THE DEFAULT VIEW NEEDS NO NETWORK. Recent months are
 * embedded in the page as base64, so a frames.html saved to a stick and
 * opened off file:// still browses. Older months are fetched only when asked
 * for, and say so plainly when there is nothing to fetch from.
 */
(function () {
  "use strict";

  // ------------------------------------------------------------- reading ---

  /** The text of a <script> payload the page embedded, or null. */
  function payload(id) {
    var el = typeof document === "undefined" ? null : document.getElementById(id);
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
    var out = "";
    for (var i = 0; i < n; i++) {
      var b = buf[at + i];
      out += (b < 16 ? "0" : "") + b.toString(16);
    }
    return out;
  }

  /** A varint at `st.at`, advancing it. Null on a truncated or silly one. */
  function varint(buf, st) {
    var shift = 0, v = 0;
    for (;;) {
      if (st.at >= buf.length) return null;
      var b = buf[st.at++];
      v += (b & 0x7f) * Math.pow(2, shift);
      if ((b & 0x80) === 0) return v;
      shift += 7;
      if (shift > 63) return null;
    }
  }

  /** One frame at `at`, or null. `end` is the byte after it. */
  function decodeFrame(buf, at) {
    var version = magicAt(buf, at);
    if (!version || at + 5 > buf.length) return null;
    var st = { at: at + 4 };
    var suspect = buf[st.at++] !== 0;
    var seq = varint(buf, st);
    var started = varint(buf, st);
    var n = varint(buf, st);
    if (seq === null || started === null || n === null) return null;
    if (n > buf.length - st.at + 1) return null; // a length that cannot fit
    var samples = [];
    for (var i = 0; i < n; i++) {
      if (st.at >= buf.length) return null;
      var tag = buf[st.at];
      if (tag === ESCAPE) {
        st.at++;
        var gap = varint(buf, st);
        var count = varint(buf, st);
        if (gap === null || count === null) return null;
        samples.push([gap, count]);
      } else if (tag === 0xff) {
        return null; // padding is never data
      } else {
        st.at++;
        samples.push([1, tag]);
      }
    }
    var klen = varint(buf, st);
    if (klen === null || st.at + klen > buf.length) return null;
    var key = hex(buf, st.at, klen);
    st.at += klen;
    var link = "";
    if (version === 2) {
      var llen = varint(buf, st);
      if (llen === null || st.at + llen > buf.length) return null;
      link = hex(buf, st.at, llen);
      st.at += llen;
    }
    return {
      seq: seq, started: started, suspect: suspect, samples: samples,
      key: key, link: link, end: st.at
    };
  }

  /**
   * Every frame in a buffer, in file order.
   *
   * A DAMAGED FRAME COSTS ONE FRAME, which is the whole reason the magic is
   * per frame rather than per file. A frame is only accepted when what
   * follows it is another magic or the end -- a corrupted length parses as a
   * different, longer frame that swallows its neighbour, and that boundary
   * check is the one test that catches a plausible-looking wrong answer.
   */
  function readFrames(buf) {
    var out = [], at = 0;
    while (at + 4 <= buf.length) {
      var f = magicAt(buf, at) ? decodeFrame(buf, at) : null;
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

  // ---------------------------------------------------------------- time ---

  function pad(n) { return (n < 10 ? "0" : "") + n; }

  function stamp(epoch) {
    var d = new Date(epoch * 1000);
    return d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate())
      + "T" + pad(d.getHours()) + ":" + pad(d.getMinutes()) + ":" + pad(d.getSeconds());
  }

  function dayOf(epoch) {
    var d = new Date(epoch * 1000);
    return d.getFullYear() + "-" + pad(d.getMonth() + 1) + "-" + pad(d.getDate());
  }

  function monthOf(epoch) {
    var d = new Date(epoch * 1000);
    return d.getFullYear() + "-" + pad(d.getMonth() + 1);
  }

  function clockOf(epoch) {
    var d = new Date(epoch * 1000);
    return pad(d.getHours()) + ":" + pad(d.getMinutes());
  }

  function fmt(v, places) {
    if (!isFinite(v)) return "--";
    return v.toFixed(places === undefined ? 2 : places);
  }

  function bytesText(n) {
    if (n >= 1024 * 1024) return (n / (1024 * 1024)).toFixed(1) + " MiB";
    if (n >= 1024) return (n / 1024).toFixed(1) + " KiB";
    return n + " B";
  }

  // ------------------------------------------------------------ spectrum ---
  //
  // THE SAME ARITHMETIC AS src/analysis.rs, DELIBERATELY. The terminal draws
  // this spectrum while the counter is running and this page draws it from
  // the record afterwards; if they disagreed, one of them would be lying
  // about the same seconds. Hann taper, half-overlapped windows, power
  // summed and divided by the number of windows, every bin against the
  // average bin -- so 1.0 is what flat looks like, here as there.

  function fft(values) {
    var n = values.length, i, j, k;
    var re = new Float64Array(n), im = new Float64Array(n);
    for (i = 0; i < n; i++) re[i] = values[i];
    j = 0;
    for (i = 1; i < n; i++) {
      var bit = n >> 1;
      while (j & bit) { j ^= bit; bit >>= 1; }
      j |= bit;
      if (i < j) {
        var tr = re[i]; re[i] = re[j]; re[j] = tr;
        var ti = im[i]; im[i] = im[j]; im[j] = ti;
      }
    }
    for (var len = 2; len <= n; len <<= 1) {
      var ang = -2 * Math.PI / len;
      var wr = Math.cos(ang), wi = Math.sin(ang);
      for (i = 0; i < n; i += len) {
        var cr = 1, ci = 0;
        for (k = 0; k < len / 2; k++) {
          var ur = re[i + k], ui = im[i + k];
          var xr = re[i + k + len / 2], xi = im[i + k + len / 2];
          var vr = xr * cr - xi * ci, vi = xr * ci + xi * cr;
          re[i + k] = ur + vr; im[i + k] = ui + vi;
          re[i + k + len / 2] = ur - vr; im[i + k + len / 2] = ui - vi;
          var ncr = cr * wr - ci * wi;
          ci = cr * wi + ci * wr;
          cr = ncr;
        }
      }
    }
    return { re: re, im: im };
  }

  function Spectrum(window) {
    this.window = window;
    this.bins = window / 2;
    this.buf = [];
    this.power = new Float64Array(window / 2);
    this.runs = 0;
    this.taper = new Float64Array(window);
    for (var i = 0; i < window; i++) {
      this.taper[i] = 0.5 - 0.5 * Math.cos(2 * Math.PI * i / (window - 1));
    }
  }

  Spectrum.prototype.add = function (counts) {
    this.buf.push(counts);
    if (this.buf.length < this.window) return false;
    var mean = 0, i;
    for (i = 0; i < this.window; i++) mean += this.buf[i];
    mean /= this.window;
    var shaped = new Float64Array(this.window);
    for (i = 0; i < this.window; i++) shaped[i] = (this.buf[i] - mean) * this.taper[i];
    var spec = fft(shaped);
    for (i = 0; i < this.bins; i++) {
      // norm() in the Rust is re*re + im*im -- power, not magnitude.
      this.power[i] += spec.re[i] * spec.re[i] + spec.im[i] * spec.im[i];
    }
    this.runs++;
    this.buf = this.buf.slice(this.window / 2);
    return true;
  };

  Spectrum.prototype.wait = function () {
    return this.runs > 0 ? 0 : this.window - this.buf.length;
  };

  /** Each bin against the average bin. Bin zero -- the mean -- is dropped. */
  Spectrum.prototype.relative = function () {
    if (this.runs === 0) return [];
    var avg = [], i;
    for (i = 1; i < this.bins; i++) avg.push(this.power[i] / this.runs);
    var mean = 0;
    for (i = 0; i < avg.length; i++) mean += avg[i];
    mean /= avg.length;
    if (!(mean > 0)) {
      return avg.map(function () { return 0; });
    }
    return avg.map(function (p) { return p / mean; });
  };

  Spectrum.prototype.loudest = function () {
    var rel = this.relative();
    if (!rel.length) return [0, 0];
    var best = [rel[0], 0];
    for (var i = 0; i < rel.length; i++) {
      if (rel[i] > best[0]) best = [rel[i], i];
    }
    return best;
  };

  Spectrum.prototype.independent = function () {
    return this.runs > 1 ? this.runs * 9 / 11 : this.runs;
  };

  /** How high the tallest bin gets by luck alone, over this many windows. */
  Spectrum.prototype.chanceMax = function () {
    var n = this.independent();
    if (n < 1 || this.bins < 2) return Infinity;
    var x = Math.log(this.bins - 1) / n;
    return 1 + Math.sqrt(2 * x) + 2 * x / 3;
  };

  Spectrum.prototype.period = function (index) {
    return this.window / (index + 1);
  };

  /**
   * The spectrum of one frame's counts, off the same ladder the recorder
   * used: the widest window with two averages in it, else the widest with
   * any, else the one closest to having one.
   */
  function spectrumOf(counts) {
    var rungs = [new Spectrum(128), new Spectrum(256), new Spectrum(512)];
    var i, k;
    for (i = 0; i < counts.length; i++) {
      for (k = 0; k < rungs.length; k++) rungs[k].add(counts[i]);
    }
    var best = null;
    for (k = rungs.length - 1; k >= 0 && !best; k--) {
      if (rungs[k].runs >= 2) best = rungs[k];
    }
    for (k = rungs.length - 1; k >= 0 && !best; k--) {
      if (rungs[k].runs > 0) best = rungs[k];
    }
    if (!best) return null;
    var loud = best.loudest();
    var chance = best.chanceMax();
    return {
      window: best.window,
      runs: best.runs,
      relative: best.relative(),
      loudest: loud,
      chanceMax: chance,
      period: best.period(loud[1]),
      // The recorder's rule, character for character.
      suspect: loud[0] > 0 && loud[0] >= chance * 1.25
    };
  }

  // ------------------------------------------------------------- shaping ---

  /** The facts about a frame that a table row or a header needs. */
  function summarise(f) {
    var counts = [], total = 0, peak = 0, gaps = 0, doubled = 0, span = 0;
    for (var i = 0; i < f.samples.length; i++) {
      var gap = f.samples[i][0], c = f.samples[i][1];
      counts.push(c);
      total += c;
      if (c > peak) peak = c;
      // A GAP IS TIME THE COUNTER WAS AWAY. A gap of zero is the opposite --
      // a second that was sampled twice -- and counting it as a gap would
      // put "the counter was away" against data that is denser, not thinner.
      if (i > 0 && gap > 1) gaps++;
      if (i > 0 && gap === 0) doubled++;
      span += gap;
    }
    return {
      frame: f,
      seq: f.seq,
      started: f.started,
      day: dayOf(f.started),
      month: monthOf(f.started),
      samples: f.samples.length,
      seconds: span,
      counts: counts,
      total: total,
      peak: peak,
      gaps: gaps,
      doubled: doubled,
      rate: span > 0 ? total / span : 0,
      suspect: f.suspect,
      key: f.key,
      link: f.link
    };
  }

  /** Rows grouped by local day, oldest first, each with its own totals. */
  function byDay(rows) {
    var days = [], index = {};
    for (var i = 0; i < rows.length; i++) {
      var d = rows[i].day;
      if (!(d in index)) {
        index[d] = days.length;
        days.push({ day: d, rows: [], counts: 0, seconds: 0, suspect: 0 });
      }
      var bucket = days[index[d]];
      bucket.rows.push(rows[i]);
      bucket.counts += rows[i].total;
      bucket.seconds += rows[i].seconds;
      if (rows[i].suspect) bucket.suspect++;
    }
    days.sort(function (a, b) { return a.day < b.day ? -1 : a.day > b.day ? 1 : 0; });
    return days;
  }

  /**
   * The counts as one character a second, gaps as dots -- the same readout
   * `radbeeper frames show` prints, so the page and the terminal can be put
   * side by side and compared by eye.
   */
  function digits(f, perLine) {
    // ONE CHARACTER A SECOND STOPS MEANING ANYTHING ABOVE 35. A counter on a
    // real source puts hundreds in a second, and a grid that renders every
    // one of them as the same `+` is not a compressed reading of the record,
    // it is a blank one. Past that, the numbers are written out.
    var peak = 0, i;
    for (i = 0; i < f.samples.length; i++) {
      if (f.samples[i][1] > peak) peak = f.samples[i][1];
    }
    if (peak > 35) return numbers(f, perLine ? Math.floor(perLine / 6) : 10);
    var width = perLine || 60;
    var out = [], line = "";
    function push(ch) {
      line += ch;
      if (line.length === width) { out.push(line); line = ""; }
    }
    for (i = 0; i < f.samples.length; i++) {
      var gap = f.samples[i][0], c = f.samples[i][1];
      if (i > 0 && gap > 1) {
        var dots = Math.min(gap, 6) - 1;
        for (var k = 0; k < dots; k++) push(".");
      }
      push(c <= 9 ? String(c) : c <= 35 ? String.fromCharCode(87 + c) : "+");
    }
    if (line) out.push(line);
    return out;
  }

  /** The counts written out, for a counter too busy for one character each. */
  function numbers(f, perLine) {
    var width = perLine || 10;
    var out = [], row = [];
    for (var i = 0; i < f.samples.length; i++) {
      if (i > 0 && f.samples[i][0] > 1) row.push("\u00b7\u00b7\u00b7");
      var cell = String(f.samples[i][1]);
      while (cell.length < 4) cell = " " + cell;
      row.push(cell);
      if (row.length >= width) { out.push(row.join(" ")); row = []; }
    }
    if (row.length) out.push(row.join(" "));
    return out;
  }

  /**
   * Where each sample sits on a time axis, in seconds from the frame's start.
   *
   * Pure, and separate from the drawing, so the zoom can be tested without a
   * canvas: the brush and the wheel move `view`, and this says what lands
   * inside it.
   */
  function series(f) {
    var out = [], t = 0;
    for (var i = 0; i < f.samples.length; i++) {
      t += i === 0 ? 0 : f.samples[i][0];
      out.push([t, f.samples[i][1], f.samples[i][0]]);
    }
    return out;
  }

  /** A view kept inside the data it is looking at, and never inside out. */
  function clampView(view, span) {
    var min = 1; // one second is as far in as this can go
    var width = Math.max(min, Math.min(view.to - view.from, span));
    var from = Math.max(0, Math.min(view.from, span - width));
    return { from: from, to: from + width };
  }

  // ------------------------------------------------------------------ ui ---

  function el(tag, cls, text) {
    var e = document.createElement(tag);
    if (cls) e.className = cls;
    if (text !== undefined && text !== null) e.textContent = String(text);
    return e;
  }

  function css(name, fallback) {
    try {
      var v = getComputedStyle(document.body).getPropertyValue(name);
      return v && v.trim() ? v.trim() : fallback;
    } catch (e) {
      return fallback;
    }
  }

  function clear(node) {
    while (node.firstChild) node.removeChild(node.firstChild);
  }

  /** A canvas sized to its box in real device pixels, and its context. */
  function fitCanvas(canvas, height) {
    var ratio = window.devicePixelRatio || 1;
    var width = canvas.clientWidth || 640;
    canvas.width = Math.round(width * ratio);
    canvas.height = Math.round(height * ratio);
    canvas.style.height = height + "px";
    var ctx = canvas.getContext("2d");
    ctx.setTransform(ratio, 0, 0, ratio, 0, 0);
    ctx.clearRect(0, 0, width, height);
    return { ctx: ctx, width: width, height: height };
  }

  function drawTime(canvas, row, view) {
    var box = fitCanvas(canvas, 170);
    var ctx = box.ctx, w = box.width, h = box.height;
    var pts = series(row.frame);
    var span = pts.length ? pts[pts.length - 1][0] : 0;
    view = clampView(view, Math.max(span, 1));
    var peak = 1;
    for (var i = 0; i < pts.length; i++) {
      if (pts[i][0] >= view.from && pts[i][0] <= view.to && pts[i][1] > peak) {
        peak = pts[i][1];
      }
    }
    var pad = 26;
    var plotH = h - pad;
    var scale = w / Math.max(1, view.to - view.from);

    ctx.fillStyle = css("--line", "#e0ddd8");
    ctx.fillRect(0, plotH, w, 1);

    // One bar a second. Gaps are drawn as the counter being away rather
    // than as a zero, because a zero is a second that was counted.
    var barW = Math.max(1, Math.min(8, scale * 0.8));
    ctx.fillStyle = css("--accent", "#2f6f4f");
    for (i = 0; i < pts.length; i++) {
      var t = pts[i][0];
      if (t < view.from || t > view.to) continue;
      var x = (t - view.from) * scale;
      var barH = (pts[i][1] / peak) * (plotH - 6);
      ctx.fillRect(x - barW / 2, plotH - barH, barW, barH);
    }
    ctx.fillStyle = css("--warn", "#b0642a");
    for (i = 1; i < pts.length; i++) {
      if (pts[i][2] <= 1) continue;
      var gx = (pts[i][0] - view.from) * scale;
      if (gx < 0 || gx > w) continue;
      ctx.fillRect(gx - 1, 0, 2, plotH);
    }

    ctx.fillStyle = css("--dim", "#6b6b6b");
    ctx.font = "11px system-ui,sans-serif";
    ctx.fillText(fmt(view.from, 0) + "s", 2, h - 8);
    var right = fmt(view.to, 0) + "s";
    ctx.fillText(right, w - ctx.measureText(right).width - 2, h - 8);
    var label = "peak " + peak + " in a second";
    ctx.fillText(label, (w - ctx.measureText(label).width) / 2, h - 8);
    return view;
  }

  function drawSpectrum(canvas, spec) {
    var box = fitCanvas(canvas, 170);
    var ctx = box.ctx, w = box.width, h = box.height;
    var pad = 26, plotH = h - pad;
    if (!spec) {
      ctx.fillStyle = css("--dim", "#6b6b6b");
      ctx.font = "12px system-ui,sans-serif";
      ctx.fillText("too few seconds for a spectrum", 4, plotH / 2);
      return;
    }
    var rel = spec.relative;
    var top = Math.max(spec.loudest[0], spec.chanceMax * 1.4, 2);
    var barW = w / rel.length;
    for (var i = 0; i < rel.length; i++) {
      var v = rel[i];
      var barH = (v / top) * (plotH - 4);
      ctx.fillStyle = v >= spec.chanceMax * 1.25 ? css("--warn", "#b0642a")
                    : css("--accent", "#2f6f4f");
      ctx.fillRect(i * barW, plotH - barH, Math.max(1, barW - 0.5), barH);
    }
    // FLAT IS 1.0, and the line luck alone reaches is the one that matters:
    // a bin above it is the reason a frame gets called suspect.
    function rule(value, colour, text) {
      if (!isFinite(value)) return;
      var y = plotH - (value / top) * (plotH - 4);
      ctx.fillStyle = colour;
      ctx.fillRect(0, y, w, 1);
      ctx.font = "11px system-ui,sans-serif";
      ctx.fillText(text, 2, Math.max(10, y - 3));
    }
    rule(1, css("--line", "#e0ddd8"), "flat");
    rule(spec.chanceMax, css("--dim", "#6b6b6b"), "luck " + fmt(spec.chanceMax));
    ctx.fillStyle = css("--dim", "#6b6b6b");
    ctx.font = "11px system-ui,sans-serif";
    ctx.fillText(spec.window + "s window", 2, h - 8);
    var right = "2s";
    ctx.fillText(right, w - ctx.measureText(right).width - 2, h - 8);
    var mid = "loudest " + fmt(spec.loudest[0]) + " at " + fmt(spec.period, 0) + "s";
    ctx.fillText(mid, (w - ctx.measureText(mid).width) / 2, h - 8);
  }

  // --------------------------------------------------------------- model ---

  function Browser(meta, host) {
    this.meta = meta;
    this.host = host;
    this.loaded = {};      // month label -> rows
    this.month = null;
    this.day = null;
    this.row = null;
    this.view = null;
    this.mode = "time";
    this.sort = { key: "started", dir: 1 };
  }

  /** Frames for a month: from the page when they are in it, else fetched. */
  Browser.prototype.load = function (label, done) {
    var self = this;
    if (this.loaded[label]) return done(null, this.loaded[label]);
    this.note("fetching " + label + " ...");
    var month = null;
    for (var i = 0; i < this.meta.months.length; i++) {
      if (this.meta.months[i].label === label) month = this.meta.months[i];
    }
    if (!month) return done("no such month");
    if (typeof fetch !== "function") {
      return done("this month is not in the page, and there is nothing to fetch from");
    }
    fetch(month.file).then(function (r) {
      if (!r.ok) throw new Error(r.status + " " + r.statusText);
      return r.arrayBuffer();
    }).then(function (b) {
      var rows = readFrames(new Uint8Array(b)).map(summarise);
      self.loaded[label] = rows;
      done(null, rows);
    })["catch"](function (e) {
      // A page opened off a disk cannot fetch, and saying which is which
      // saves somebody assuming their record is gone.
      done("could not read " + month.file + " (" + e.message + ")");
    });
  };

  Browser.prototype.select = function (label) {
    var self = this;
    this.month = label;
    this.day = null;
    this.row = null;
    this.render();
    this.load(label, function (err, rows) {
      if (err) {
        self.note(err);
        return;
      }
      if (self.month !== label) return; // somebody clicked on while it loaded
      if (self.wanted && self.wanted.month === label) {
        var want = self.wanted;
        self.wanted = null;
        if (want.mode) self.mode = want.mode;
        for (var i = 0; i < rows.length; i++) {
          if (rows[i].seq === want.seq) {
            self.open(rows[i]);
            return;
          }
        }
        self.note("no frame " + want.seq + " in " + label);
      }
      self.render();
    });
  };

  Browser.prototype.note = function (text) {
    var box = this.host.querySelector(".rb-note");
    if (box) box.textContent = text || "";
  };

  Browser.prototype.rows = function () {
    return this.loaded[this.month] || [];
  };

  Browser.prototype.shown = function () {
    var rows = this.rows();
    if (this.day) {
      rows = rows.filter(function (r) { return r.day === this.day; }, this);
    }
    var key = this.sort.key, dir = this.sort.dir;
    return rows.slice().sort(function (a, b) {
      var x = a[key], y = b[key];
      if (x === y) return a.started - b.started;
      return (x < y ? -1 : 1) * dir;
    });
  };

  Browser.prototype.open = function (row) {
    this.row = row;
    var span = row.seconds || 1;
    this.view = { from: 0, to: span };
    // A FRAME IS A THING YOU CAN LINK TO. An audit trail people quote at
    // each other needs an address per frame, not just per page -- and the
    // address is the month and the seq, which is what identifies a frame
    // everywhere else in this program too.
    this.address(row);
    this.render();
  };

  Browser.prototype.address = function (row) {
    if (typeof history === "undefined" || !history.replaceState) return;
    try {
      history.replaceState(null, "", "#" + this.month + "/" + row.seq
                                     + (this.mode === "spectrum" ? "/spectrum" : ""));
    } catch (e) {
      /* a file:// URL in some browsers refuses this; the page still works */
    }
  };

  /**
   * `#2026-09/12` -> that frame; `#2026-09/12/spectrum` -> that frame, in
   * that view. The view is part of the address because "look at this" and
   * "look at this spectrum" are different things to send somebody.
   */
  function readHash(hash) {
    var m = /^#([0-9]{4}-[0-9]{2}|undated)\/([0-9]+)(?:\/(time|spectrum))?$/
              .exec(hash || "");
    if (!m) return null;
    return { month: m[1] === "undated" ? "" : m[1], seq: Number(m[2]),
             mode: m[3] || null };
  }

  // ---------------------------------------------------------------- draw ---

  Browser.prototype.render = function () {
    var self = this;
    var host = this.host;
    clear(host);

    // The months.
    var rail = el("div", "rb-rail");
    this.meta.months.forEach(function (m) {
      var b = el("button", "rb-chip" + (m.label === self.month ? " on" : ""));
      b.appendChild(el("span", "rb-chip-label", m.label || "undated"));
      b.appendChild(el("span", "rb-chip-sub",
                       m.frames + " frames · " + bytesText(m.bytes)
                       + (m.embedded ? "" : " · fetched")));
      b.onclick = function () { self.select(m.label); };
      rail.appendChild(b);
    });
    host.appendChild(rail);
    host.appendChild(el("p", "rb-note sub", ""));

    var rows = this.rows();
    if (!rows.length) {
      host.appendChild(el("p", "sub", "Nothing loaded for this month yet."));
      return;
    }

    // The days in the month, as a strip of bars: the shape of the record
    // before any of it is read.
    var days = byDay(rows);
    var busiest = days.reduce(function (m, d) { return Math.max(m, d.rows.length); }, 1);
    var strip = el("div", "rb-days");
    var all = el("button", "rb-day rb-day-all" + (this.day ? "" : " on"));
    all.appendChild(el("span", "rb-day-label", "all"));
    all.onclick = function () { self.day = null; self.render(); };
    strip.appendChild(all);
    days.forEach(function (d) {
      var b = el("button", "rb-day" + (d.day === self.day ? " on" : "")
                           + (d.suspect ? " warn" : ""));
      var bar = el("span", "rb-day-bar");
      bar.style.height = Math.round(6 + 26 * d.rows.length / busiest) + "px";
      b.appendChild(bar);
      b.appendChild(el("span", "rb-day-label", d.day.slice(8)));
      b.title = d.day + ": " + d.rows.length + " frames, " + d.counts
                + " counts" + (d.suspect ? ", " + d.suspect + " suspect" : "");
      b.onclick = function () { self.day = d.day; self.row = null; self.render(); };
      strip.appendChild(b);
    });
    host.appendChild(strip);

    // The frames themselves.
    var shown = this.shown();
    var table = el("table", "rb-table");
    var head = el("tr");
    [["seq", "seq"], ["started", "started"], ["seconds", "seconds"],
     ["samples", "samples"], ["total", "counts"], ["peak", "peak"],
     ["rate", "a second"], ["gaps", "gaps"], ["suspect", "spectrum"]]
      .forEach(function (col) {
        var th = el("th", "rb-sortable", col[1]);
        if (self.sort.key === col[0]) {
          th.className += " on";
          th.textContent = col[1] + (self.sort.dir > 0 ? " ↑" : " ↓");
        }
        th.onclick = function () {
          if (self.sort.key === col[0]) self.sort.dir = -self.sort.dir;
          else self.sort = { key: col[0], dir: 1 };
          self.render();
        };
        head.appendChild(th);
      });
    table.appendChild(head);
    shown.forEach(function (r) {
      var tr = el("tr", "rb-row" + (self.row === r ? " on" : ""));
      [r.seq, stamp(r.started).replace("T", " "), r.seconds + "s", r.samples,
       r.total, r.peak, fmt(r.rate), r.gaps || "",
       r.suspect ? "not flat" : "flat"].forEach(function (v, i) {
        var td = el("td", i === 8 && r.suspect ? "rb-warn" : null, v);
        tr.appendChild(td);
      });
      tr.onclick = function () { self.open(r); };
      table.appendChild(tr);
    });
    var scroll = el("div", "rb-scroll");
    scroll.appendChild(table);
    host.appendChild(scroll);

    if (this.row) host.appendChild(this.inspector());
  };

  Browser.prototype.inspector = function () {
    var self = this;
    var row = this.row;
    var box = el("section", "rb-inspect");
    box.appendChild(el("h3", null, "Frame " + row.seq + " · "
                                   + stamp(row.started).replace("T", " ")));

    var facts = el("dl", "rb-facts");
    function fact(k, v, cls) {
      facts.appendChild(el("dt", null, k));
      facts.appendChild(el("dd", cls || null, v));
    }
    fact("covers", row.seconds + " seconds in " + row.samples + " samples");
    fact("counts", row.total + " · " + fmt(row.rate) + " a second · peak " + row.peak);
    fact("gaps", row.gaps ? row.gaps + " (the counter was away)" : "none");
    if (row.doubled) {
      fact("doubled", row.doubled + " sample"
                      + (row.doubled === 1 ? "" : "s")
                      + " landed inside a second already sampled");
    }
    fact("key", row.key);
    fact("link", row.link || "— written before the chain");
    box.appendChild(facts);

    var tabs = el("div", "rb-tabs");
    [["time", "second by second"], ["spectrum", "spectrum"]].forEach(function (t) {
      var b = el("button", "rb-tab" + (self.mode === t[0] ? " on" : ""), t[1]);
      b.onclick = function () {
        self.mode = t[0];
        if (self.row) self.address(self.row);
        self.render();
      };
      tabs.appendChild(b);
    });
    box.appendChild(tabs);

    var canvas = el("canvas", "rb-canvas");
    box.appendChild(canvas);
    var caption = el("p", "sub rb-caption", "");
    box.appendChild(caption);

    var spec = this.mode === "spectrum" ? spectrumOf(row.counts) : null;
    // The canvas has no width until it is in the document, so the drawing
    // waits for the layout rather than guessing at one.
    setTimeout(function () {
      if (self.mode === "time") {
        self.view = drawTime(canvas, row, self.view || { from: 0, to: row.seconds || 1 });
        caption.textContent = "Drag to pan, wheel to zoom. Orange marks a gap "
          + "where the counter was away.";
      } else {
        drawSpectrum(canvas, spec);
        caption.textContent = spec
          ? (spec.suspect
              ? "NOT FLAT: a bin stands above what luck reaches, so this frame "
                + "was recorded suspect."
              : "Flat, which is the good answer: no period stands out of "
                + spec.runs + " averaged windows.")
          : "This frame is shorter than the smallest window.";
        if (spec && spec.suspect !== row.suspect) {
          // The flag came off the monitor's running ladder -- every second
          // that process had seen, both tubes summed. This is one frame.
          // They answer different questions and may differ.
          caption.textContent += " The recorder wrote suspect=" + row.suspect
            + " from the whole run, where a period longer than this frame is "
            + "visible; this frame on its own gives " + spec.suspect + ".";
        }
      }
    }, 0);

    if (this.mode === "time") {
      canvas.onwheel = function (e) {
        e.preventDefault();
        var span = row.seconds || 1;
        var v = self.view || { from: 0, to: span };
        var at = v.from + (e.offsetX / canvas.clientWidth) * (v.to - v.from);
        var factor = e.deltaY > 0 ? 1.25 : 0.8;
        var width = (v.to - v.from) * factor;
        self.view = clampView({ from: at - (at - v.from) * factor,
                                to: at - (at - v.from) * factor + width }, span);
        self.view = drawTime(canvas, row, self.view);
      };
      var dragging = null;
      canvas.onpointerdown = function (e) {
        dragging = { x: e.offsetX, view: self.view };
        canvas.setPointerCapture(e.pointerId);
      };
      canvas.onpointermove = function (e) {
        if (!dragging) return;
        var span = row.seconds || 1;
        var width = dragging.view.to - dragging.view.from;
        var moved = (e.offsetX - dragging.x) / canvas.clientWidth * width;
        self.view = clampView({ from: dragging.view.from - moved,
                                to: dragging.view.to - moved }, span);
        drawTime(canvas, row, self.view);
      };
      canvas.onpointerup = function () { dragging = null; };
    }

    var pre = el("pre", "rb-digits", digits(row.frame).join("\n"));
    // The readout has two forms and the caption has to be the one that is
    // actually below it: a busy counter gets its counts written out.
    box.appendChild(el("p", "sub", row.peak > 35
      ? "Every second, written out — ten to a line, and ··· where the "
        + "counter was away."
      : "Every second, one character each — 0-9 then a-z, and a dot where "
        + "the counter was away."));
    box.appendChild(pre);
    return box;
  };

  // ---------------------------------------------------------------- boot ---

  function boot() {
    var host = document.getElementById("rb-browser");
    if (!host) return;
    var meta;
    try {
      meta = JSON.parse(payload("rb-manifest") || "{}");
    } catch (e) {
      host.appendChild(el("p", "sub", "The manifest in this page did not parse."));
      return;
    }
    if (!meta.months || !meta.months.length) {
      host.appendChild(el("p", "sub", "This counter has no frame files yet."));
      return;
    }
    var browser = new Browser(meta, host);
    // What the page carries is decoded once, then split by the month each
    // frame actually started in -- the file it came out of is the same
    // answer, and this way one concatenated blob needs no index.
    var embedded = readFrames(unbase64(payload("rb-frames") || ""));
    var rows = embedded.map(summarise);
    for (var i = 0; i < rows.length; i++) {
      var m = rows[i].month;
      if (!browser.loaded[m]) browser.loaded[m] = [];
      browser.loaded[m].push(rows[i]);
    }
    var want = readHash(typeof location === "undefined" ? "" : location.hash);
    var newest = meta.months[meta.months.length - 1].label;
    var start = newest;
    if (want) {
      for (var k = 0; k < meta.months.length; k++) {
        if (meta.months[k].label === want.month) start = want.month;
      }
      browser.wanted = want;
    }
    browser.select(start);
  }

  if (typeof document !== "undefined") {
    if (document.readyState === "loading") {
      document.addEventListener("DOMContentLoaded", boot);
    } else {
      boot();
    }
  }

  // Exposed for the test harness, which runs this file under node with no DOM.
  if (typeof module !== "undefined" && module.exports) {
    module.exports = { readFrames: readFrames, decodeFrame: decodeFrame,
                       numbers: numbers, readHash: readHash,
                       spectrumOf: spectrumOf, Spectrum: Spectrum, fft: fft,
                       summarise: summarise, byDay: byDay, digits: digits,
                       series: series, clampView: clampView, dayOf: dayOf,
                       monthOf: monthOf, bytesText: bytesText };
  }
})();
