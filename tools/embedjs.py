#!/usr/bin/env python3
"""Paste src/frames.js into the reference implementation, between markers.

WHY THIS EXISTS. random.html carries a frame viewer, and random.html is
compared byte for byte against the page ./radbeeper writes -- so both programs
have to emit the same javascript, to the character. The Rust does it with
include_str!, which cannot drift. The Python cannot: it is installed as a
single file with nothing beside it, so it has to carry a copy.

A copy that somebody maintains by hand is a copy that is wrong within a month,
so nobody maintains this one. This tool writes it, and tests/test_frames_js.py
fails the suite when the two differ -- which turns "I forgot" from a bug that
ships into a red test.

    python3 tools/embedjs.py            # rewrite the block
    python3 tools/embedjs.py --check    # exit 1 if it is stale
"""
import os
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
SOURCE = os.path.join(ROOT, "src", "frames.js")
TARGET = os.path.join(ROOT, "radbeeper")

OPEN = "# ---8<--- src/frames.js begins; written by tools/embedjs.py ---8<---"
CLOSE = "# --->8--- src/frames.js ends ---8<---"

# Spelt rather than written, so this file can talk about the sequence it
# emits without ending its own strings.
QUOTES = chr(34) * 3


def block(js):
    # A RAW TRIPLE-QUOTED STRING, and the javascript is concatenated to the
    # opening quotes with NOTHING IN BETWEEN -- no newline, and above all no
    # backslash continuation. In a raw string a trailing backslash is a
    # literal backslash, so opening with a continuation puts one at the head
    # of the value. The Rust's include_str! does not, and random.html is
    # compared byte for byte, so that one character failed the whole
    # differential suite.
    #
    # The trailing newline is kept for the same reason: include_str! keeps the
    # file's last newline, so this has to as well.
    #
    # It reads acceptably in the file because the javascript opens with a
    # block comment.
    return OPEN + '\nVIEWER_JS = r' + QUOTES + js + QUOTES + '\n' + CLOSE


def verify(js):
    """Refuse to embed javascript that cannot survive the quoting."""
    if '"""' in js:
        raise SystemExit("src/frames.js contains a triple quote, which the "
                         "embedded block cannot hold")
    if js.endswith("\\"):
        raise SystemExit("src/frames.js ends in a backslash, which would "
                         "escape the closing quote")
    if js[:1] == '"':
        raise SystemExit("src/frames.js opens with a quote, which would close "
                         "the embedded string immediately")
    if not js.endswith("\n"):
        raise SystemExit("src/frames.js does not end in a newline, so the "
                         "embedded copy could not match include_str!")


def main(argv):
    with open(SOURCE, encoding="utf-8") as f:
        js = f.read()
    verify(js)
    with open(TARGET, encoding="utf-8") as f:
        text = f.read()
    if OPEN not in text or CLOSE not in text:
        raise SystemExit("no marker block in %s -- add one first" % TARGET)
    head, rest = text.split(OPEN, 1)
    _stale, tail = rest.split(CLOSE, 1)
    fresh = head + block(js) + tail
    if "--check" in argv:
        if fresh != text:
            print("radbeeper's copy of src/frames.js is stale -- run "
                  "python3 tools/embedjs.py", file=sys.stderr)
            return 1
        print("embedded javascript is current")
        return 0
    if fresh == text:
        print("already current")
        return 0
    with open(TARGET, "w", encoding="utf-8") as f:
        f.write(fresh)
    print("embedded %d bytes of src/frames.js into radbeeper" % len(js))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
